use std::path::PathBuf;
use std::time::{Duration, Instant};

use ruler_core::{
    capture::{create_backend, CapturedFrame},
    PixelFormat, RulerConfig,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetKind {
    MuMu,
    LDPlayer,
    Windows,
    Adb,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LatencyClass {
    PaleGreen,
    Green,
    Yellow,
    Orange,
    Red,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct PreviewFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

#[derive(Clone, Debug)]
pub struct TargetCandidate {
    pub kind: TargetKind,
    pub fingerprint: String,
    pub name: String,
    pub detail: String,
    pub config: RulerConfig,
    pub latency: Option<Duration>,
    pub latency_class: LatencyClass,
    pub preview: Option<PreviewFrame>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ProbeResult {
    pub fingerprint: String,
    pub latency: Option<Duration>,
    pub latency_class: LatencyClass,
    pub preview: Option<PreviewFrame>,
    pub error: Option<String>,
}

impl TargetCandidate {
    #[must_use]
    pub fn latency_text(&self) -> String {
        match self.latency {
            Some(latency) => format!("{:.1} ms", latency.as_secs_f64() * 1000.0),
            None => "measuring...".to_string(),
        }
    }

    #[must_use]
    pub fn list_label(&self) -> String {
        let status = self
            .error
            .as_ref()
            .map(|error| format!("error: {error}"))
            .unwrap_or_else(|| self.latency_text());
        format!(
            "{} | {} | {} | {}",
            kind_label(self.kind),
            self.name,
            status,
            self.detail
        )
    }
}

fn kind_label(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::MuMu => "MuMu",
        TargetKind::LDPlayer => "LDPlayer",
        TargetKind::Windows => "Windows",
        TargetKind::Adb => "ADB",
    }
}

#[must_use]
pub fn latency_class(latency: Duration) -> LatencyClass {
    let ms = latency.as_secs_f64() * 1000.0;
    if ms < 8.0 {
        LatencyClass::PaleGreen
    } else if ms <= 16.0 {
        LatencyClass::Green
    } else if ms <= 33.0 {
        LatencyClass::Yellow
    } else if ms <= 166.0 {
        LatencyClass::Orange
    } else {
        LatencyClass::Red
    }
}

#[must_use]
pub fn discover_targets(previous: Option<&RulerConfig>) -> Vec<TargetCandidate> {
    platform::discover_targets(previous)
}

/// Discover `adb.exe` paths bundled with running MuMu / LDPlayer emulators.
///
/// Returned paths are absolute and de-duplicated. The list is fed into
/// `ruler_core::capture::adb_resolver::resolve_adb_with` so that adb-using
/// capture backends fall back to a bundled adb when none is on `PATH`.
///
/// On non-Windows this always returns an empty vector (MuMu / LDPlayer are
/// Windows-only).
#[must_use]
pub fn discover_emulator_adb_paths() -> Vec<PathBuf> {
    platform::discover_emulator_adb_paths()
}

#[must_use]
pub fn probe_candidate_once(candidate: &TargetCandidate) -> ProbeResult {
    probe_config_once(candidate.fingerprint.clone(), &candidate.config)
}

#[must_use]
pub fn probe_config_once(fingerprint: String, config: &RulerConfig) -> ProbeResult {
    let capture_config = match config.to_capture_config() {
        Ok(config) => config,
        Err(error) => {
            return ProbeResult {
                fingerprint,
                latency: None,
                latency_class: LatencyClass::Unknown,
                preview: None,
                error: Some(error.to_string()),
            };
        }
    };
    let mut backend = match create_backend(capture_config) {
        Ok(backend) => backend,
        Err(error) => {
            return ProbeResult {
                fingerprint,
                latency: None,
                latency_class: LatencyClass::Unknown,
                preview: None,
                error: Some(error),
            };
        }
    };

    let result = (|| {
        backend.connect()?;
        let start = Instant::now();
        let frame = backend.capture_frame()?;
        let latency = start.elapsed();
        Ok::<_, String>((latency, frame))
    })();
    backend.disconnect();

    match result {
        Ok((latency, frame)) => ProbeResult {
            fingerprint,
            latency: Some(latency),
            latency_class: latency_class(latency),
            preview: Some(preview_from_frame(frame)),
            error: None,
        },
        Err(error) => ProbeResult {
            fingerprint,
            latency: None,
            latency_class: LatencyClass::Unknown,
            preview: None,
            error: Some(error),
        },
    }
}

fn preview_from_frame(frame: CapturedFrame) -> PreviewFrame {
    PreviewFrame {
        data: frame.data,
        width: frame.width,
        height: frame.height,
        format: frame.format,
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub fn discover_targets(_: Option<&RulerConfig>) -> Vec<TargetCandidate> {
        Vec::new()
    }

    pub fn discover_emulator_adb_paths() -> Vec<PathBuf> {
        Vec::new()
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        collections::{HashMap, HashSet},
        path::{Path, PathBuf},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    use netstat2::{
        get_sockets_info, AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState,
    };
    use ruler_core::capture::adb_resolver::adb_command;
    use ruler_core::RulerConfig;
    use serde_json::Value;
    use sysinfo::System;
    use windows::Win32::{
        Foundation::{BOOL, HWND, LPARAM},
        Globalization::{MultiByteToWideChar, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, CP_ACP},
        UI::WindowsAndMessaging::{
            EnumWindows, GetClassNameW, GetWindowTextLengthW, GetWindowTextW,
            GetWindowThreadProcessId, IsWindowVisible,
        },
    };

    use super::{LatencyClass, TargetCandidate, TargetKind};

    const COMMAND_TIMEOUT: Duration = Duration::from_millis(1800);
    const PACKAGE_NAMES: &[&str] = &[
        "com.hypergryph.arknights",
        "com.hypergryph.arknights.bilibili",
        "tw.txwy.and.arknights",
        "com.YoStarEN.Arknights",
        "com.YoStarJP.Arknights",
        "com.YoStarKR.Arknights",
    ];

    #[derive(Clone, Debug)]
    struct ProcessInfo {
        name: String,
        process_id: u32,
        parent_process_id: Option<u32>,
        executable_path: Option<String>,
        command_line: Option<String>,
        creation_time: u64,
    }

    #[derive(Clone, Copy, Debug)]
    struct TcpListener {
        port: u16,
        pid: u32,
    }

    #[derive(Clone, Debug)]
    struct WindowInfo {
        hwnd: isize,
        title: String,
        class_name: String,
    }

    #[derive(Clone, Debug)]
    struct MuMuInstall {
        install_path: PathBuf,
        manager_path: PathBuf,
    }

    #[derive(Clone, Debug)]
    struct MuMuManagerInfo {
        index: u32,
        name: Option<String>,
        host: String,
        port: u16,
    }

    #[derive(Clone, Debug)]
    struct LDPlayerInstance {
        index: u32,
        name: String,
        player_pid: u32,
        vbox_pid: u32,
    }

    pub fn discover_targets(previous: Option<&RulerConfig>) -> Vec<TargetCandidate> {
        let processes = query_processes();
        let process_by_pid = processes
            .iter()
            .map(|process| (process.process_id, process.clone()))
            .collect::<HashMap<_, _>>();
        let listeners = query_tcp_listeners();
        let connected_adb_serials = adb_connected_serials();
        let mut claimed_ports = HashSet::new();
        let mut claimed_serials = HashSet::new();
        let mut candidates = Vec::new();

        discover_mumu(
            &processes,
            &process_by_pid,
            &listeners,
            &connected_adb_serials,
            previous,
            &mut claimed_ports,
            &mut claimed_serials,
            &mut candidates,
        );
        discover_ldplayer(
            &processes,
            &process_by_pid,
            &listeners,
            &connected_adb_serials,
            previous,
            &mut claimed_ports,
            &mut claimed_serials,
            &mut candidates,
        );
        discover_windows(&processes, previous, &mut candidates);
        discover_generic_adb(
            &connected_adb_serials,
            previous,
            &claimed_ports,
            &claimed_serials,
            &mut candidates,
        );

        let mut seen = HashSet::new();
        candidates
            .into_iter()
            .filter(|candidate| seen.insert(candidate.fingerprint.clone()))
            .collect()
    }

    /// Walk running processes for MuMu / LDPlayer install roots and return
    /// each bundled `adb.exe` found under them. De-duplicated, absolute paths.
    ///
    /// This feeds `ruler_core::capture::adb_resolver::resolve_adb_with` so
    /// the wizard and capture backends can fall back to a bundled adb when
    /// none is on `PATH`. Modeled after MaaFramework's emulator-bundled adb
    /// lookup.
    pub fn discover_emulator_adb_paths() -> Vec<PathBuf> {
        let processes = query_processes();
        let mut paths: Vec<PathBuf> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        let mumu_relatives: &[&str] = &[
            "shell\\adb.exe",
            "nx_main\\adb.exe",
            "nx_device\\12.0\\shell\\adb.exe",
            "adb.exe",
        ];
        // Pull install roots both from the MuMu manager tool scan (covers
        // `MuMuManager.exe` and friends) and from any running headless VM
        // process (covers the case where the manager isn't running but the
        // emulator itself is).
        let mut mumu_roots: Vec<PathBuf> = discover_mumu_tool_installs(&processes)
            .into_iter()
            .map(|install| install.install_path)
            .collect();
        for process in processes
            .iter()
            .filter(|process| process.name.eq_ignore_ascii_case("MuMuVMMHeadless.exe"))
        {
            if let Some(exe) = process.executable_path.as_deref() {
                if let Some(root) = resolve_mumu_install_path(exe) {
                    mumu_roots.push(root);
                }
            }
        }
        for root in mumu_roots {
            for relative in mumu_relatives {
                push_unique_adb(&mut paths, &mut seen, root.join(relative));
            }
        }

        // LDPlayer: install root is the directory containing both
        // `dnconsole.exe` and `ldopengl64.dll`; `adb.exe` lives at the root.
        let mut ldplayer_roots: Vec<PathBuf> = discover_ldplayer_tool_installs(&processes);
        for process in processes
            .iter()
            .filter(|process| process.name.eq_ignore_ascii_case("Ld9BoxHeadless.exe"))
        {
            if let Some(exe) = process.executable_path.as_deref() {
                if let Some(root) = resolve_ldplayer_install_path(exe) {
                    ldplayer_roots.push(root);
                }
            }
        }
        for root in ldplayer_roots {
            push_unique_adb(&mut paths, &mut seen, root.join("adb.exe"));
        }

        paths
    }

    fn push_unique_adb(paths: &mut Vec<PathBuf>, seen: &mut HashSet<String>, candidate: PathBuf) {
        if !candidate.is_file() {
            return;
        }
        let key = candidate.to_string_lossy().to_ascii_lowercase();
        if seen.insert(key) {
            paths.push(candidate);
        }
    }

    fn discover_mumu(
        processes: &[ProcessInfo],
        process_by_pid: &HashMap<u32, ProcessInfo>,
        listeners: &[TcpListener],
        connected_adb_serials: &[String],
        previous: Option<&RulerConfig>,
        claimed_ports: &mut HashSet<u16>,
        claimed_serials: &mut HashSet<String>,
        candidates: &mut Vec<TargetCandidate>,
    ) {
        for install in discover_mumu_tool_installs(processes) {
            let program = install.manager_path.to_string_lossy().into_owned();
            let Ok(output) =
                run_command_text(&program, &["info", "--vmindex", "all"], COMMAND_TIMEOUT)
            else {
                continue;
            };

            for info in parse_mumu_manager_infos(&output) {
                let Some(serial) =
                    adb_serial_for_address(&info.host, info.port, connected_adb_serials)
                else {
                    continue;
                };
                if !adb_has_arknights_package(&serial) {
                    continue;
                }

                claimed_ports.insert(info.port);
                claimed_serials.insert(serial.clone());
                push_mumu_candidate(
                    install.install_path.clone(),
                    info.index,
                    info.name.clone(),
                    serial,
                    previous,
                    candidates,
                );
            }
        }

        for process in processes
            .iter()
            .filter(|process| process.name.eq_ignore_ascii_case("MuMuVMMHeadless.exe"))
        {
            let Some(executable_path) = process.executable_path.as_deref() else {
                continue;
            };
            let Some(install_path) = resolve_mumu_install_path_for_process(process, processes)
                .or_else(|| resolve_mumu_install_path(executable_path))
            else {
                continue;
            };
            let instance_index = parse_mumu_instance(process)
                .or_else(|| parse_mumu_instance_from_related_process(process, processes))
                .unwrap_or(0);

            let ports = related_listener_ports(
                process,
                Some(&install_path),
                processes,
                process_by_pid,
                listeners,
            );
            let Some((serial, port)) = adb_serial_for_ports(&ports, connected_adb_serials)
                .find(|(serial, _)| adb_has_arknights_package(serial))
            else {
                continue;
            };
            claimed_ports.insert(port);
            claimed_serials.insert(serial.clone());
            push_mumu_candidate(
                install_path,
                instance_index,
                None,
                serial,
                previous,
                candidates,
            );
        }
    }

    fn discover_ldplayer(
        processes: &[ProcessInfo],
        process_by_pid: &HashMap<u32, ProcessInfo>,
        listeners: &[TcpListener],
        connected_adb_serials: &[String],
        previous: Option<&RulerConfig>,
        claimed_ports: &mut HashSet<u16>,
        claimed_serials: &mut HashSet<String>,
        candidates: &mut Vec<TargetCandidate>,
    ) {
        for install_path in discover_ldplayer_tool_installs(processes) {
            for instance in ldplayer_instances_for_install(&install_path) {
                if instance.player_pid == 0 || instance.vbox_pid == 0 {
                    continue;
                }

                let mut ports = listeners
                    .iter()
                    .filter(|listener| {
                        listener.pid == instance.player_pid || listener.pid == instance.vbox_pid
                    })
                    .map(|listener| listener.port)
                    .filter(|port| *port != 5037 && *port >= 1024)
                    .collect::<Vec<_>>();
                ports.sort_unstable();
                ports.dedup();

                let mut matched_port = None;
                if let Some(serial) =
                    ldplayer_adb_serial_for_instance(instance.index, connected_adb_serials)
                        .filter(|serial| adb_has_arknights_package(serial))
                {
                    matched_port = Some((serial, 5554 + instance.index as u16 * 2));
                }
                if matched_port.is_none() {
                    for (serial, port) in adb_serial_for_ports(&ports, connected_adb_serials) {
                        if adb_has_arknights_package(&serial) {
                            matched_port = Some((serial, port));
                            break;
                        }
                    }
                }

                let Some((serial, port)) = matched_port else {
                    continue;
                };
                claimed_ports.insert(port);
                claimed_serials.insert(serial.clone());
                push_ldplayer_candidate(
                    install_path.clone(),
                    instance.index,
                    Some(instance.name.clone()),
                    serial,
                    previous,
                    candidates,
                );
            }
        }

        for process in processes
            .iter()
            .filter(|process| process.name.eq_ignore_ascii_case("Ld9BoxHeadless.exe"))
        {
            let Some(executable_path) = process.executable_path.as_deref() else {
                continue;
            };
            let Some(install_path) = resolve_ldplayer_install_path_for_process(process, processes)
                .or_else(|| resolve_ldplayer_install_path(executable_path))
            else {
                continue;
            };
            let Some(instance_index) = ldplayer_instance_for_pid(&install_path, process.process_id)
            else {
                continue;
            };

            let vbox_pids = matching_vbox_pids(process, instance_index, processes, process_by_pid);
            let mut ports = related_listener_ports(
                process,
                Some(&install_path),
                processes,
                process_by_pid,
                listeners,
            );
            ports.extend(
                listeners
                    .iter()
                    .filter(|listener| vbox_pids.contains(&listener.pid))
                    .map(|listener| listener.port),
            );
            ports.sort_unstable();
            ports.dedup();
            let mut matched_port = None;
            if let Some(serial) =
                ldplayer_adb_serial_for_instance(instance_index, connected_adb_serials)
                    .filter(|serial| adb_has_arknights_package(serial))
            {
                matched_port = Some((serial, 5554 + instance_index as u16 * 2));
            }
            if matched_port.is_none() {
                for (serial, port) in adb_serial_for_ports(&ports, connected_adb_serials) {
                    if adb_has_arknights_package(&serial) {
                        matched_port = Some((serial, port));
                        break;
                    }
                }
            }

            let Some((serial, port)) = matched_port else {
                continue;
            };
            claimed_ports.insert(port);
            claimed_serials.insert(serial.clone());
            push_ldplayer_candidate(
                install_path,
                instance_index,
                None,
                serial,
                previous,
                candidates,
            );
        }
    }

    fn discover_windows(
        processes: &[ProcessInfo],
        previous: Option<&RulerConfig>,
        candidates: &mut Vec<TargetCandidate>,
    ) {
        for process in processes
            .iter()
            .filter(|process| process.name.eq_ignore_ascii_case("Arknights.exe"))
        {
            for window in windows_for_pid(process.process_id) {
                let executable = process.executable_path.as_deref().unwrap_or_default();
                let fingerprint = format!(
                    "window:{}:{}:{}",
                    stable_path(Path::new(executable)),
                    window.title,
                    window.class_name
                );
                let mut config = base_config(
                    "window",
                    None,
                    None,
                    None,
                    Some(window.hwnd),
                    Some(window.title.clone()),
                    Some(window.class_name.clone()),
                    &fingerprint,
                    previous,
                );
                config.target_fingerprint = Some(fingerprint.clone());

                candidates.push(TargetCandidate {
                    kind: TargetKind::Windows,
                    fingerprint,
                    name: "Windows Arknights".to_string(),
                    detail: format!("{} [{}]", window.title, window.class_name),
                    config,
                    latency: None,
                    latency_class: LatencyClass::Unknown,
                    preview: None,
                    error: None,
                });
            }
        }
    }

    fn discover_generic_adb(
        connected_adb_serials: &[String],
        previous: Option<&RulerConfig>,
        claimed_ports: &HashSet<u16>,
        claimed_serials: &HashSet<String>,
        candidates: &mut Vec<TargetCandidate>,
    ) {
        let mut seen_serials = HashSet::new();
        for serial in connected_adb_serials {
            if claimed_serials.contains(serial) {
                continue;
            }
            if serial_port(&serial).is_some_and(|port| claimed_ports.contains(&port)) {
                continue;
            }
            if adb_has_arknights_package(&serial) && seen_serials.insert(serial.clone()) {
                push_generic_adb_candidate(serial.clone(), previous, candidates);
            }
        }
    }

    fn push_generic_adb_candidate(
        serial: String,
        previous: Option<&RulerConfig>,
        candidates: &mut Vec<TargetCandidate>,
    ) {
        let fingerprint = format!("adb:{serial}");
        let mut config = base_config(
            "adb",
            None,
            None,
            Some(serial.clone()),
            None,
            None,
            None,
            &fingerprint,
            previous,
        );
        config.target_fingerprint = Some(fingerprint.clone());

        candidates.push(TargetCandidate {
            kind: TargetKind::Adb,
            fingerprint,
            name: "Generic ADB".to_string(),
            detail: serial,
            config,
            latency: None,
            latency_class: LatencyClass::Unknown,
            preview: None,
            error: None,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn base_config(
        capture_type: &str,
        install_path: Option<String>,
        instance_index: Option<u32>,
        device_id: Option<String>,
        window_handle: Option<isize>,
        window_title: Option<String>,
        window_class: Option<String>,
        fingerprint: &str,
        previous: Option<&RulerConfig>,
    ) -> RulerConfig {
        RulerConfig {
            capture_type: capture_type.to_string(),
            install_path,
            instance_index,
            device_id,
            window_handle,
            window_title,
            window_class,
            active_calibration_profile: previous
                .and_then(|config| config.active_calibration_profile.clone()),
            frame_display_mode: previous
                .and_then(|config| config.frame_display_mode.clone())
                .or_else(|| Some("0_to_n-1".to_string())),
            language: previous
                .and_then(|config| config.language.clone())
                .or_else(|| Some("zh_CN".to_string())),
            auto_select_target: false,
            target_fingerprint: Some(fingerprint.to_string()),
            overlay_pos_x: previous.and_then(|config| config.overlay_pos_x),
            overlay_pos_y: previous.and_then(|config| config.overlay_pos_y),
            overlay_scale: previous.and_then(|config| config.overlay_scale),
            ui_scaler: previous.and_then(|config| config.ui_scaler),
            debug_recording_enabled: previous.map(|c| c.debug_recording_enabled).unwrap_or(false),
            debug_recording_video: previous.map(|c| c.debug_recording_video).unwrap_or(false),
            debug_recording_csv: previous.map(|c| c.debug_recording_csv).unwrap_or(false),
            trace_logging_enabled: previous.map(|c| c.trace_logging_enabled).unwrap_or(false),
            log_output_dir: previous.and_then(|c| c.log_output_dir.clone()),
            replay_hevc_path: previous.and_then(|c| c.replay_hevc_path.clone()),
            replay_fps: previous.and_then(|c| c.replay_fps),
        }
    }

    fn push_mumu_candidate(
        install_path: PathBuf,
        instance_index: u32,
        display_name: Option<String>,
        serial: String,
        previous: Option<&RulerConfig>,
        candidates: &mut Vec<TargetCandidate>,
    ) {
        let install_text = install_path.to_string_lossy().into_owned();
        let fingerprint = format!("mumu:{}:{}", stable_path(&install_path), instance_index);
        let mut config = base_config(
            "mumu",
            Some(install_text),
            Some(instance_index),
            Some(serial.clone()),
            None,
            None,
            None,
            &fingerprint,
            previous,
        );
        config.target_fingerprint = Some(fingerprint.clone());

        candidates.push(TargetCandidate {
            kind: TargetKind::MuMu,
            fingerprint,
            name: non_empty(display_name).unwrap_or_else(|| format!("MuMu #{instance_index}")),
            detail: format!("{serial} | {}", install_path.display()),
            config,
            latency: None,
            latency_class: LatencyClass::Unknown,
            preview: None,
            error: None,
        });
    }

    fn push_ldplayer_candidate(
        install_path: PathBuf,
        instance_index: u32,
        display_name: Option<String>,
        serial: String,
        previous: Option<&RulerConfig>,
        candidates: &mut Vec<TargetCandidate>,
    ) {
        let install_text = install_path.to_string_lossy().into_owned();
        let fingerprint = format!("ldplayer:{}:{}", stable_path(&install_path), instance_index);
        let mut config = base_config(
            "ldplayer",
            Some(install_text),
            Some(instance_index),
            Some(serial.clone()),
            None,
            None,
            None,
            &fingerprint,
            previous,
        );
        config.target_fingerprint = Some(fingerprint.clone());

        candidates.push(TargetCandidate {
            kind: TargetKind::LDPlayer,
            fingerprint,
            name: non_empty(display_name).unwrap_or_else(|| format!("LDPlayer #{instance_index}")),
            detail: format!("{serial} | {}", install_path.display()),
            config,
            latency: None,
            latency_class: LatencyClass::Unknown,
            preview: None,
            error: None,
        });
    }

    fn non_empty(value: Option<String>) -> Option<String> {
        value
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    fn discover_mumu_tool_installs(processes: &[ProcessInfo]) -> Vec<MuMuInstall> {
        let mut seen = HashSet::new();
        let mut installs = Vec::new();
        for process in processes
            .iter()
            .filter(|process| is_mumu_discovery_process(&process.name))
        {
            let Some(install_path) = resolve_mumu_install_path_for_process(process, processes)
                .or_else(|| {
                    process
                        .executable_path
                        .as_deref()
                        .and_then(resolve_mumu_install_path)
                })
            else {
                continue;
            };
            let Some(manager_path) = resolve_mumu_manager_path(&install_path, process) else {
                continue;
            };
            if seen.insert(stable_path(&manager_path)) {
                installs.push(MuMuInstall {
                    install_path,
                    manager_path,
                });
            }
        }
        installs
    }

    fn is_mumu_discovery_process(name: &str) -> bool {
        [
            "MuMuPlayer.exe",
            "MuMuNxDevice.exe",
            "MuMuVMMHeadless.exe",
            "NemuPlayer.exe",
        ]
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
    }

    fn resolve_mumu_manager_path(install_path: &Path, process: &ProcessInfo) -> Option<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(parent) = process
            .executable_path
            .as_deref()
            .and_then(|path| Path::new(path).parent())
        {
            candidates.push(parent.join("MuMuManager.exe"));
        }
        candidates.extend([
            install_path.join("shell").join("MuMuManager.exe"),
            install_path.join("nx_main").join("MuMuManager.exe"),
            install_path
                .join("nx_device")
                .join("12.0")
                .join("shell")
                .join("MuMuManager.exe"),
            install_path.join("MuMuManager.exe"),
        ]);
        candidates.into_iter().find(|path| path.exists())
    }

    fn parse_mumu_manager_infos(output: &str) -> Vec<MuMuManagerInfo> {
        let Ok(value) = serde_json::from_str::<Value>(output) else {
            return Vec::new();
        };

        match value {
            Value::Array(values) => values
                .iter()
                .filter_map(parse_mumu_manager_info_value)
                .collect(),
            Value::Object(map) if is_mumu_info_object(&map) => {
                parse_mumu_manager_info_value(&Value::Object(map))
                    .into_iter()
                    .collect()
            }
            Value::Object(map) => map
                .values()
                .filter_map(parse_mumu_manager_info_value)
                .collect(),
            _ => Vec::new(),
        }
    }

    fn is_mumu_info_object(map: &serde_json::Map<String, Value>) -> bool {
        map.contains_key("adb_port") || map.contains_key("adb_host_ip") || map.contains_key("index")
    }

    fn parse_mumu_manager_info_value(value: &Value) -> Option<MuMuManagerInfo> {
        let map = value.as_object()?;
        let index = json_u32(map.get("index")?)?;
        let port = json_u16(map.get("adb_port")?)?;
        if port == 0 {
            return None;
        }

        let host = map
            .get("adb_host_ip")
            .and_then(json_string)
            .map(|host| normalize_adb_host(&host))
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let name = map.get("name").and_then(json_string);

        Some(MuMuManagerInfo {
            index,
            name,
            host,
            port,
        })
    }

    fn json_u32(value: &Value) -> Option<u32> {
        match value {
            Value::Number(number) => number.as_u64().and_then(|value| u32::try_from(value).ok()),
            Value::String(text) => text.trim().parse::<u32>().ok(),
            _ => None,
        }
    }

    fn json_u16(value: &Value) -> Option<u16> {
        json_u32(value).and_then(|value| u16::try_from(value).ok())
    }

    fn json_string(value: &Value) -> Option<String> {
        match value {
            Value::String(text) => Some(text.trim().to_string()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        }
        .filter(|value| !value.is_empty())
    }

    fn discover_ldplayer_tool_installs(processes: &[ProcessInfo]) -> Vec<PathBuf> {
        let mut seen = HashSet::new();
        let mut installs = Vec::new();
        for process in processes
            .iter()
            .filter(|process| is_ldplayer_discovery_process(&process.name))
        {
            let Some(install_path) = resolve_ldplayer_install_path_for_process(process, processes)
            else {
                continue;
            };
            if seen.insert(stable_path(&install_path)) {
                installs.push(install_path);
            }
        }
        installs
    }

    fn is_ldplayer_discovery_process(name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        lower == "dnplayer.exe"
            || lower == "ldplayer.exe"
            || lower == "ld9boxheadless.exe"
            || lower == "ldboxheadless.exe"
            || lower == "ldvboxheadless.exe"
            || lower.contains("dnplayer")
    }

    fn ldplayer_instances_for_install(install_path: &Path) -> Vec<LDPlayerInstance> {
        let dnconsole = install_path.join("dnconsole.exe");
        let Ok(output) =
            run_dnconsole_text(&dnconsole.to_string_lossy(), &["list2"], COMMAND_TIMEOUT)
        else {
            return Vec::new();
        };
        parse_ldplayer_instances(&output)
    }

    fn parse_ldplayer_instances(output: &str) -> Vec<LDPlayerInstance> {
        output
            .lines()
            .filter_map(|line| {
                let parts = line.split(',').map(str::trim).collect::<Vec<_>>();
                if parts.len() < 7 {
                    return None;
                }
                Some(LDPlayerInstance {
                    index: parts[0].parse::<u32>().ok()?,
                    name: parts[1].to_string(),
                    player_pid: parts[5].parse::<u32>().ok()?,
                    vbox_pid: parts[6].parse::<u32>().ok()?,
                })
            })
            .collect()
    }

    fn query_processes() -> Vec<ProcessInfo> {
        System::new_all()
            .processes()
            .values()
            .map(|process| ProcessInfo {
                name: process.name().to_string_lossy().into_owned(),
                process_id: process.pid().as_u32(),
                parent_process_id: process.parent().map(|pid| pid.as_u32()),
                executable_path: process
                    .exe()
                    .map(|path| path.to_string_lossy().into_owned()),
                command_line: (!process.cmd().is_empty()).then(|| {
                    process
                        .cmd()
                        .iter()
                        .map(|part| part.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" ")
                }),
                creation_time: process.start_time(),
            })
            .collect()
    }

    fn query_tcp_listeners() -> Vec<TcpListener> {
        let Ok(sockets) = get_sockets_info(AddressFamilyFlags::all(), ProtocolFlags::TCP) else {
            return Vec::new();
        };
        sockets
            .into_iter()
            .flat_map(|socket| {
                let port = match socket.protocol_socket_info {
                    ProtocolSocketInfo::Tcp(tcp) if tcp.state == TcpState::Listen => {
                        Some(tcp.local_port)
                    }
                    _ => None,
                };
                port.into_iter().flat_map(move |port| {
                    socket
                        .associated_pids
                        .clone()
                        .into_iter()
                        .map(move |pid| TcpListener { port, pid })
                })
            })
            .collect()
    }

    fn resolve_mumu_install_path(executable_path: &str) -> Option<PathBuf> {
        let mut current = Path::new(executable_path).parent()?;
        let mut found = None;
        loop {
            if mumu_dll_exists(current) {
                found = Some(current.to_path_buf());
            }
            let Some(parent) = current.parent() else {
                return found;
            };
            current = parent;
        }
    }

    fn resolve_mumu_install_path_for_process(
        process: &ProcessInfo,
        processes: &[ProcessInfo],
    ) -> Option<PathBuf> {
        process
            .executable_path
            .as_deref()
            .and_then(resolve_mumu_install_path)
            .or_else(|| {
                let instance_name = parse_mumu_instance_name(process);
                processes
                    .iter()
                    .filter(|candidate| {
                        candidate.name.to_ascii_lowercase().contains("mumu")
                            || candidate
                                .executable_path
                                .as_deref()
                                .is_some_and(|path| path.to_ascii_lowercase().contains("mumu"))
                    })
                    .filter(|candidate| {
                        instance_name.as_ref().is_none_or(|instance_name| {
                            candidate
                                .command_line
                                .as_deref()
                                .is_some_and(|line| line.contains(instance_name))
                                || candidate
                                    .executable_path
                                    .as_deref()
                                    .is_some_and(|path| path.contains(instance_name))
                                || candidate.name.eq_ignore_ascii_case("MuMuNxDevice.exe")
                        })
                    })
                    .filter_map(|candidate| {
                        candidate
                            .executable_path
                            .as_deref()
                            .and_then(resolve_mumu_install_path)
                    })
                    .next()
            })
    }

    fn mumu_dll_exists(base: &Path) -> bool {
        [
            "nx_device/12.0/shell/sdk/external_renderer_ipc.dll",
            "nx_main/sdk/external_renderer_ipc.dll",
            "shell/sdk/external_renderer_ipc.dll",
        ]
        .iter()
        .any(|relative| base.join(relative).exists())
    }

    fn parse_mumu_instance(process: &ProcessInfo) -> Option<u32> {
        let text = format!(
            "{} {}",
            process.command_line.as_deref().unwrap_or_default(),
            process.executable_path.as_deref().unwrap_or_default()
        );
        parse_named_number(&text, &["instance", "instance_index", "index", "playerid"])
            .or_else(|| parse_trailing_segment_number(&text, "12.0-"))
            .or_else(|| parse_trailing_segment_number(&text, "mumu-"))
    }

    fn parse_mumu_instance_name(process: &ProcessInfo) -> Option<String> {
        let text = format!(
            "{} {}",
            process.command_line.as_deref().unwrap_or_default(),
            process.executable_path.as_deref().unwrap_or_default()
        );
        text.split(|ch: char| ch.is_whitespace() || ch == '"' || ch == '\\' || ch == '/')
            .find(|part| part.starts_with("MuMuPlayer-12.0-"))
            .map(str::to_string)
    }

    fn parse_mumu_instance_from_related_process(
        headless: &ProcessInfo,
        processes: &[ProcessInfo],
    ) -> Option<u32> {
        let instance_name = parse_mumu_instance_name(headless);
        processes
            .iter()
            .filter(|process| {
                process.name.to_ascii_lowercase().contains("mumu")
                    || process
                        .executable_path
                        .as_deref()
                        .is_some_and(|path| path.to_ascii_lowercase().contains("mumu"))
                    || process
                        .command_line
                        .as_deref()
                        .is_some_and(|line| line.contains("MuMuPlayer-12.0-"))
            })
            .filter(|process| {
                instance_name.as_ref().is_none_or(|name| {
                    process
                        .command_line
                        .as_deref()
                        .is_some_and(|line| line.contains(name))
                        || process
                            .executable_path
                            .as_deref()
                            .is_some_and(|path| path.contains(name))
                })
            })
            .find_map(parse_mumu_instance)
            .or_else(|| {
                instance_name
                    .as_deref()
                    .and_then(|name| parse_trailing_segment_number(name, "12.0-"))
            })
    }

    fn resolve_ldplayer_install_path(executable_path: &str) -> Option<PathBuf> {
        let mut current = Path::new(executable_path).parent()?;
        loop {
            if current.join("dnconsole.exe").exists() && current.join("ldopengl64.dll").exists() {
                return Some(current.to_path_buf());
            }
            current = current.parent()?;
        }
    }

    fn resolve_ldplayer_install_path_for_process(
        process: &ProcessInfo,
        processes: &[ProcessInfo],
    ) -> Option<PathBuf> {
        process
            .executable_path
            .as_deref()
            .and_then(resolve_ldplayer_install_path)
            .or_else(|| {
                processes
                    .iter()
                    .filter(|candidate| {
                        let name = candidate.name.to_ascii_lowercase();
                        name.contains("ld")
                            || name.contains("dn")
                            || candidate.executable_path.as_deref().is_some_and(|path| {
                                let path = path.to_ascii_lowercase();
                                path.contains("ldplayer") || path.contains("leidian")
                            })
                    })
                    .filter_map(|candidate| {
                        candidate
                            .executable_path
                            .as_deref()
                            .and_then(resolve_ldplayer_install_path)
                    })
                    .next()
            })
    }

    fn ldplayer_instance_for_pid(install_path: &Path, pid: u32) -> Option<u32> {
        let dnconsole = install_path.join("dnconsole.exe");
        let output =
            run_dnconsole_text(&dnconsole.to_string_lossy(), &["list2"], COMMAND_TIMEOUT).ok()?;
        parse_ldplayer_instance_for_pid(&output, pid)
    }

    fn parse_ldplayer_instance_for_pid(output: &str, pid: u32) -> Option<u32> {
        output.lines().find_map(|line| {
            let parts = line.split(',').map(str::trim).collect::<Vec<_>>();
            if parts.len() < 6 {
                return None;
            }
            let dnplayer_pid = parts.get(5).and_then(|value| value.parse::<u32>().ok());
            let headless_pid = parts.get(6).and_then(|value| value.parse::<u32>().ok());
            if dnplayer_pid != Some(pid) && headless_pid != Some(pid) {
                return None;
            }
            parts[0].parse::<u32>().ok()
        })
    }

    fn matching_vbox_pids(
        ld_process: &ProcessInfo,
        instance_index: u32,
        processes: &[ProcessInfo],
        process_by_pid: &HashMap<u32, ProcessInfo>,
    ) -> HashSet<u32> {
        let token = instance_index.to_string();
        processes
            .iter()
            .filter(|process| process.name.eq_ignore_ascii_case("VBoxNetNAT.exe"))
            .filter(|process| {
                process
                    .command_line
                    .as_deref()
                    .is_some_and(|line| line.contains(&token))
                    || shares_ancestor(ld_process, process, process_by_pid)
                    || near_creation_time(ld_process, process)
            })
            .map(|process| process.process_id)
            .collect()
    }

    fn related_listener_ports(
        root: &ProcessInfo,
        install_path: Option<&Path>,
        processes: &[ProcessInfo],
        process_by_pid: &HashMap<u32, ProcessInfo>,
        listeners: &[TcpListener],
    ) -> Vec<u16> {
        let related_pids = related_process_pids(root, install_path, processes, process_by_pid);
        let mut ports = listeners
            .iter()
            .filter(|listener| related_pids.contains(&listener.pid))
            .map(|listener| listener.port)
            .filter(|port| *port != 5037 && *port >= 1024)
            .collect::<Vec<_>>();
        ports.sort_unstable();
        ports.dedup();
        ports
    }

    fn related_process_pids(
        root: &ProcessInfo,
        install_path: Option<&Path>,
        processes: &[ProcessInfo],
        process_by_pid: &HashMap<u32, ProcessInfo>,
    ) -> HashSet<u32> {
        processes
            .iter()
            .filter(|process| {
                process.process_id == root.process_id
                    || process.parent_process_id == Some(root.process_id)
                    || shares_ancestor(root, process, process_by_pid)
                    || install_path
                        .is_some_and(|install_path| process_under_path(process, install_path))
            })
            .map(|process| process.process_id)
            .collect()
    }

    fn process_under_path(process: &ProcessInfo, root: &Path) -> bool {
        process
            .executable_path
            .as_deref()
            .map(Path::new)
            .is_some_and(|path| path.starts_with(root))
    }

    fn shares_ancestor(
        left: &ProcessInfo,
        right: &ProcessInfo,
        process_by_pid: &HashMap<u32, ProcessInfo>,
    ) -> bool {
        let left_ancestors = ancestors(left, process_by_pid);
        let right_ancestors = ancestors(right, process_by_pid);
        left_ancestors
            .iter()
            .any(|pid| right_ancestors.contains(pid))
    }

    fn ancestors(
        process: &ProcessInfo,
        process_by_pid: &HashMap<u32, ProcessInfo>,
    ) -> HashSet<u32> {
        let mut result = HashSet::new();
        let mut current = process.parent_process_id;
        for _ in 0..8 {
            let Some(pid) = current else {
                break;
            };
            if !result.insert(pid) {
                break;
            }
            current = process_by_pid
                .get(&pid)
                .and_then(|parent| parent.parent_process_id);
        }
        result
    }

    fn near_creation_time(left: &ProcessInfo, right: &ProcessInfo) -> bool {
        left.creation_time.abs_diff(right.creation_time) <= 60
    }

    fn adb_serial_for_ports<'a>(
        ports: &'a [u16],
        connected_adb_serials: &'a [String],
    ) -> impl Iterator<Item = (String, u16)> + 'a {
        let connected = connected_adb_serials.iter().filter_map(move |serial| {
            let port = serial_port(serial)?;
            ports.contains(&port).then(|| (serial.clone(), port))
        });
        let connected_ports = connected_adb_serials
            .iter()
            .filter_map(|serial| serial_port(serial))
            .collect::<HashSet<_>>();
        let probed = ports
            .iter()
            .copied()
            .filter(move |port| !connected_ports.contains(port))
            .filter_map(move |port| {
                adb_serial_for_port(port, connected_adb_serials).map(|serial| (serial, port))
            });
        connected.chain(probed)
    }

    fn adb_serial_for_port(port: u16, connected_adb_serials: &[String]) -> Option<String> {
        adb_serial_for_address("127.0.0.1", port, connected_adb_serials)
    }

    fn adb_serial_for_address(
        host: &str,
        port: u16,
        connected_adb_serials: &[String],
    ) -> Option<String> {
        let host = normalize_adb_host(host);
        let serial = format!("{host}:{port}");
        if !connected_adb_serials
            .iter()
            .any(|connected| connected == &serial)
        {
            let _ = run_adb_text(&["connect", &serial], COMMAND_TIMEOUT).ok()?;
        }
        let state = run_adb_text(&["-s", &serial, "get-state"], COMMAND_TIMEOUT).ok()?;
        (state.trim() == "device").then_some(serial)
    }

    fn normalize_adb_host(host: &str) -> String {
        match host.trim() {
            "" | "0.0.0.0" | "localhost" => "127.0.0.1".to_string(),
            host => host.to_string(),
        }
    }

    fn ldplayer_adb_serial_for_instance(
        instance_index: u32,
        connected_adb_serials: &[String],
    ) -> Option<String> {
        let serial = format!("emulator-{}", 5554 + instance_index * 2);
        connected_adb_serials
            .iter()
            .any(|connected| connected == &serial)
            .then_some(serial)
    }

    fn adb_connected_serials() -> Vec<String> {
        let Ok(output) = run_adb_text(&["devices"], COMMAND_TIMEOUT) else {
            return Vec::new();
        };
        output
            .lines()
            .skip(1)
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let serial = parts.next()?;
                let state = parts.next()?;
                (state == "device").then(|| serial.to_string())
            })
            .collect()
    }

    fn serial_port(serial: &str) -> Option<u16> {
        serial.rsplit(':').next()?.parse::<u16>().ok()
    }

    fn adb_has_arknights_package(serial: &str) -> bool {
        PACKAGE_NAMES.iter().any(|package| {
            run_adb_text(
                &["-s", serial, "shell", "pm", "path", package],
                COMMAND_TIMEOUT,
            )
            .map(|output| output.contains("package:"))
            .unwrap_or(false)
        })
    }

    fn windows_for_pid(pid: u32) -> Vec<WindowInfo> {
        let mut context = WindowSearchContext {
            pid,
            windows: Vec::new(),
        };
        unsafe {
            let _ = EnumWindows(
                Some(enum_window_proc),
                LPARAM((&mut context as *mut WindowSearchContext) as isize),
            );
        }
        context.windows.sort_by_key(window_priority);
        context.windows
    }

    struct WindowSearchContext {
        pid: u32,
        windows: Vec<WindowInfo>,
    }

    unsafe extern "system" fn enum_window_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        let context = &mut *(lparam.0 as *mut WindowSearchContext);
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != context.pid {
            return BOOL(1);
        }
        let title = window_text(hwnd);
        if title.trim().is_empty() {
            return BOOL(1);
        }
        context.windows.push(WindowInfo {
            hwnd: hwnd.0 as isize,
            title,
            class_name: window_class(hwnd),
        });
        BOOL(1)
    }

    fn window_priority(candidate: &WindowInfo) -> (u8, String) {
        let title = candidate.title.to_ascii_lowercase();
        let class_name = candidate.class_name.to_ascii_lowercase();
        let priority = if candidate.title == "明日方舟" {
            0
        } else if candidate.title.contains("明日方舟") {
            1
        } else if title.contains("arknights") {
            2
        } else if class_name == "unitywndclass" || class_name == "unityhwndclass" {
            3
        } else {
            4
        };
        (priority, candidate.title.clone())
    }

    unsafe fn window_text(hwnd: HWND) -> String {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let count = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..count as usize])
    }

    unsafe fn window_class(hwnd: HWND) -> String {
        let mut buf = [0u16; 256];
        let count = GetClassNameW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..count as usize])
    }

    fn run_command_text(program: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
        log::trace!("target discovery command: {} {}", program, args.join(" "));
        let mut command = Command::new(program);
        configure_hidden_command(&mut command);
        let bytes = run_command_capture(&mut command, program, args, timeout)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_string())
    }

    /// Same as `run_command_text` but uses the resolved adb executable
    /// (PATH or emulator-bundled) instead of a literal `"adb"` lookup.
    /// adb/MuMu output is UTF-8, so decode as such.
    fn run_adb_text(args: &[&str], timeout: Duration) -> Result<String, String> {
        log::trace!("target discovery adb command: adb {}", args.join(" "));
        let mut command = adb_command()?;
        let bytes = run_command_capture(&mut command, "adb", args, timeout)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_string())
    }

    /// Run `dnconsole.exe` and decode its output as the system ANSI code page
    /// (GBK / CP 936 on zh_CN). `dnconsole list2` writes GBK bytes, so a naive
    /// UTF-8 decode turns Chinese instance names into replacement characters.
    /// MuMu's `MuMuManager.exe` output is UTF-8 and must stay on `run_command_text`.
    fn run_dnconsole_text(program: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
        log::trace!("target discovery dnconsole command: {} {}", program, args.join(" "));
        let mut command = Command::new(program);
        configure_hidden_command(&mut command);
        let bytes = run_command_capture(&mut command, program, args, timeout)?;
        Ok(decode_ansi(&bytes).trim().to_string())
    }

    fn run_command_capture(
        command: &mut Command,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<Vec<u8>, String> {
        let mut child = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("failed to start {program}: {error}"))?;
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() < timeout => thread::sleep(Duration::from_millis(20)),
                Ok(None) => {
                    let _ = child.kill();
                    break;
                }
                Err(error) => return Err(format!("failed to wait for {program}: {error}")),
            }
        }
        let output = child
            .wait_with_output()
            .map_err(|error| format!("failed to collect {program} output: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "{} failed: {}",
                program,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(output.stdout)
    }

    /// Decode bytes from the system ANSI code page (CP_ACP, e.g. GBK/936 on
    /// zh_CN hosts) into a Rust `String` via `MultiByteToWideChar` → UTF-16 →
    /// UTF-8. Empty input yields an empty string.
    fn decode_ansi(bytes: &[u8]) -> String {
        if bytes.is_empty() {
            return String::new();
        }
        // First call with a null output buffer returns the required wide-char
        // count (cbMultiByte is taken from the input slice length, no NUL).
        let wide_len = unsafe {
            MultiByteToWideChar(
                CP_ACP,
                MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0),
                bytes,
                None,
            )
        };
        if wide_len <= 0 {
            // Fall back to a lossy UTF-8 decode so callers still get *something*.
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut wide = vec![0u16; wide_len as usize];
        let written = unsafe {
            MultiByteToWideChar(
                CP_ACP,
                MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0),
                bytes,
                Some(&mut wide),
            )
        };
        if written <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        String::from_utf16_lossy(&wide[..written as usize])
    }

    fn configure_hidden_command(command: &mut Command) {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;

            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
    }

    fn parse_named_number(text: &str, names: &[&str]) -> Option<u32> {
        let lower = text.to_ascii_lowercase();
        for name in names {
            if let Some(index) = lower.find(name) {
                if let Some(value) = first_number_after(&lower[index + name.len()..]) {
                    return Some(value);
                }
            }
        }
        None
    }

    fn first_number_after(text: &str) -> Option<u32> {
        let mut digits = String::new();
        let mut seen_separator = false;
        for ch in text.chars() {
            if ch.is_ascii_digit() {
                digits.push(ch);
            } else if digits.is_empty()
                && (ch == '-' || ch == '_' || ch == '=' || ch == ':' || ch.is_whitespace())
            {
                seen_separator = true;
            } else if !digits.is_empty() {
                break;
            } else if seen_separator {
                return None;
            }
        }
        (!digits.is_empty())
            .then(|| digits.parse::<u32>().ok())
            .flatten()
    }

    fn parse_trailing_segment_number(text: &str, marker: &str) -> Option<u32> {
        let lower = text.to_ascii_lowercase();
        let marker = marker.to_ascii_lowercase();
        let index = lower.rfind(&marker)?;
        first_number_after(&lower[index + marker.len()..])
    }

    fn stable_path(path: &Path) -> String {
        path.to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn matches_ldplayer_pid_from_list2() {
            let output = "0,name,top,1,0,1234,4321,0,0\n1,name2,top,1,0,5678,8765,0,0";
            assert_eq!(parse_ldplayer_instance_for_pid(output, 5678), Some(1));
            assert_eq!(parse_ldplayer_instance_for_pid(output, 4321), Some(0));
        }

        #[test]
        fn parses_ldplayer_instances_from_list2() {
            let output = "0,雷电模拟器,2032678,1704928,1,7456,3500,1280,720,240\n\
                          1,雷电模拟器-1,852422,590830,1,3772,3180,1920,1080,280";
            let instances = parse_ldplayer_instances(output);

            assert_eq!(instances.len(), 2);
            assert_eq!(instances[0].index, 0);
            assert_eq!(instances[0].name, "雷电模拟器");
            assert_eq!(instances[0].player_pid, 7456);
            assert_eq!(instances[0].vbox_pid, 3500);
            assert_eq!(instances[1].index, 1);
        }

        #[test]
        fn decodes_gbk_dnconsole_bytes_as_chinese() {
            // "雷电模拟器" encoded in GBK (code page 936), as `dnconsole list2`
            // would emit on a zh_CN host. A naive UTF-8 decode yields mojibake.
            let gbk = b"\xc0\xd7\xb5\xe7\xc4\xa3\xc4\xe2\xc6\xf7";
            assert_eq!(decode_ansi(gbk), "雷电模拟器");
        }

        #[test]
        fn decodes_ansi_empty_input() {
            assert_eq!(decode_ansi(b""), "");
        }

        #[test]
        fn parses_mumu_manager_object_output() {
            let output = r#"{
                "0": {
                    "index": "0",
                    "name": "主模拟器",
                    "adb_host_ip": "localhost",
                    "adb_port": 16384
                },
                "1": {
                    "index": 1,
                    "name": "副模拟器",
                    "adb_host_ip": "127.0.0.1",
                    "adb_port": "16416"
                }
            }"#;
            let infos = parse_mumu_manager_infos(output);

            assert_eq!(infos.len(), 2);
            assert_eq!(infos[0].index, 0);
            assert_eq!(infos[0].name.as_deref(), Some("主模拟器"));
            assert_eq!(infos[0].host, "127.0.0.1");
            assert_eq!(infos[0].port, 16384);
            assert_eq!(infos[1].index, 1);
            assert_eq!(infos[1].port, 16416);
        }

        #[test]
        fn parses_mumu_manager_single_output() {
            let output =
                r#"{"index":"2","name":"MuMu-2","adb_host_ip":"0.0.0.0","adb_port":16448}"#;
            let infos = parse_mumu_manager_infos(output);

            assert_eq!(infos.len(), 1);
            assert_eq!(infos[0].index, 2);
            assert_eq!(infos[0].host, "127.0.0.1");
            assert_eq!(infos[0].port, 16448);
        }

        #[test]
        fn parses_mumu_instance_markers() {
            let process = ProcessInfo {
                name: "MuMuVMMHeadless.exe".to_string(),
                process_id: 1,
                parent_process_id: None,
                executable_path: Some("D:\\MuMu\\vms\\MuMuPlayer-12.0-3\\x.exe".to_string()),
                command_line: Some("--instance_index=2".to_string()),
                creation_time: 100,
            };
            assert_eq!(parse_mumu_instance(&process), Some(2));
        }

        #[test]
        fn related_listener_ports_include_same_install_root_processes() {
            let root = process(
                "MuMuVMMHeadless.exe",
                10,
                None,
                Some("D:\\MuMu\\shell\\MuMuVMMHeadless.exe"),
            );
            let adb = process("adb.exe", 20, None, Some("D:\\MuMu\\adb.exe"));
            let unrelated = process("adb.exe", 30, None, Some("D:\\Other\\adb.exe"));
            let processes = vec![root.clone(), adb.clone(), unrelated];
            let process_by_pid = processes
                .iter()
                .map(|process| (process.process_id, process.clone()))
                .collect::<HashMap<_, _>>();
            let listeners = vec![
                TcpListener {
                    port: 16384,
                    pid: 20,
                },
                TcpListener {
                    port: 21503,
                    pid: 30,
                },
            ];

            assert_eq!(
                related_listener_ports(
                    &root,
                    Some(Path::new("D:\\MuMu")),
                    &processes,
                    &process_by_pid,
                    &listeners,
                ),
                vec![16384]
            );
        }

        #[test]
        fn mumu_install_path_can_come_from_device_process_when_headless_is_hypervisor() {
            let root = std::env::temp_dir().join(unique_test_dir("mumu-install"));
            let dll = root
                .join("nx_device")
                .join("12.0")
                .join("shell")
                .join("sdk")
                .join("external_renderer_ipc.dll");
            std::fs::create_dir_all(dll.parent().unwrap()).unwrap();
            std::fs::write(&dll, []).unwrap();

            let headless = process(
                "MuMuVMMHeadless.exe",
                1,
                None,
                Some("C:\\Program Files\\MuMuVMMVbox\\Hypervisor\\MuMuVMMHeadless.exe"),
            );
            let mut device = process(
                "MuMuNxDevice.exe",
                2,
                None,
                Some(
                    &root
                        .join("nx_device\\12.0\\shell\\MuMuNxDevice.exe")
                        .to_string_lossy(),
                ),
            );
            device.command_line = Some(format!(
                "{}\\vms\\MuMuPlayer-12.0-0\\logs\\VBox.log",
                root.display()
            ));
            let resolved =
                resolve_mumu_install_path_for_process(&headless, &[headless.clone(), device])
                    .unwrap();

            assert_eq!(resolved, root);
        }

        #[test]
        fn mumu_manager_path_can_come_from_v5_layout() {
            let root = std::env::temp_dir().join(unique_test_dir("mumu-v5-manager"));
            let shell = root.join("nx_device").join("12.0").join("shell");
            std::fs::create_dir_all(&shell).unwrap();
            std::fs::write(shell.join("MuMuManager.exe"), []).unwrap();

            let process = process(
                "MuMuNxDevice.exe",
                1,
                None,
                Some(&shell.join("MuMuNxDevice.exe").to_string_lossy()),
            );
            let resolved = resolve_mumu_manager_path(&root, &process).unwrap();

            assert_eq!(resolved, shell.join("MuMuManager.exe"));
        }

        #[test]
        fn ldplayer_install_path_can_come_from_dnplayer_when_headless_is_box() {
            let root = std::env::temp_dir().join(unique_test_dir("ld-install"));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("dnconsole.exe"), []).unwrap();
            std::fs::write(root.join("ldopengl64.dll"), []).unwrap();

            let headless = process(
                "Ld9BoxHeadless.exe",
                1,
                None,
                Some("C:\\Program Files\\ldplayer9box\\Ld9BoxHeadless.exe"),
            );
            let dnplayer = process(
                "dnplayer.exe",
                2,
                None,
                Some(&root.join("dnplayer.exe").to_string_lossy()),
            );
            let resolved =
                resolve_ldplayer_install_path_for_process(&headless, &[headless.clone(), dnplayer])
                    .unwrap();

            assert_eq!(resolved, root);
        }

        fn unique_test_dir(prefix: &str) -> String {
            format!(
                "arknights-ruler-{prefix}-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )
        }

        fn process(
            name: &str,
            process_id: u32,
            parent_process_id: Option<u32>,
            executable_path: Option<&str>,
        ) -> ProcessInfo {
            ProcessInfo {
                name: name.to_string(),
                process_id,
                parent_process_id,
                executable_path: executable_path.map(str::to_string),
                command_line: None,
                creation_time: 100,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn latency_boundaries_match_plan() {
        assert_eq!(
            latency_class(Duration::from_micros(7999)),
            LatencyClass::PaleGreen
        );
        assert_eq!(latency_class(Duration::from_millis(8)), LatencyClass::Green);
        assert_eq!(
            latency_class(Duration::from_millis(16)),
            LatencyClass::Green
        );
        assert_eq!(
            latency_class(Duration::from_millis(17)),
            LatencyClass::Yellow
        );
        assert_eq!(
            latency_class(Duration::from_millis(33)),
            LatencyClass::Yellow
        );
        assert_eq!(
            latency_class(Duration::from_millis(34)),
            LatencyClass::Orange
        );
        assert_eq!(
            latency_class(Duration::from_millis(166)),
            LatencyClass::Orange
        );
        assert_eq!(latency_class(Duration::from_millis(167)), LatencyClass::Red);
    }
}
