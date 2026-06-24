//! Windows target discovery: enumerate MuMu / LDPlayer / Windows / generic-ADB
//! capture targets running Arknights.
//!
//! This module is the windows-only `platform` backing for the cross-platform
//! API in [`super`]. It owns the shared process/TCP snapshot types and the
//! candidate-building helpers; the per-backend discovery logic lives in the
//! [`mumu`], [`ldplayer`], [`windows`], [`adb`], and [`process`] submodules.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ruler_core::RulerConfig;

use super::TargetCandidate;

mod adb;
mod ldplayer;
mod mumu;
mod process;
mod windows;

#[cfg(test)]
mod test_support;

pub(super) const COMMAND_TIMEOUT: Duration = Duration::from_millis(1800);

/// A snapshot of one running process, captured once per discovery pass.
#[derive(Clone, Debug)]
pub(super) struct ProcessInfo {
    pub(super) name: String,
    pub(super) process_id: u32,
    pub(super) parent_process_id: Option<u32>,
    pub(super) executable_path: Option<String>,
    pub(super) command_line: Option<String>,
    pub(super) creation_time: u64,
}

/// A TCP socket in the `LISTEN` state, paired with its owning PID.
#[derive(Clone, Copy, Debug)]
pub(super) struct TcpListener {
    pub(super) port: u16,
    pub(super) pid: u32,
}

pub fn discover_targets(previous: Option<&RulerConfig>) -> Vec<TargetCandidate> {
    let processes = process::query_processes();
    let process_by_pid = processes
        .iter()
        .map(|process| (process.process_id, process.clone()))
        .collect::<HashMap<_, _>>();
    let listeners = process::query_tcp_listeners();
    let connected_adb_serials = adb::adb_connected_serials();
    let mut claimed_ports = HashSet::new();
    let mut claimed_serials = HashSet::new();
    let mut candidates = Vec::new();

    mumu::discover_mumu(
        &processes,
        &process_by_pid,
        &listeners,
        &connected_adb_serials,
        previous,
        &mut claimed_ports,
        &mut claimed_serials,
        &mut candidates,
    );
    ldplayer::discover_ldplayer(
        &processes,
        &process_by_pid,
        &listeners,
        &connected_adb_serials,
        previous,
        &mut claimed_ports,
        &mut claimed_serials,
        &mut candidates,
    );
    windows::discover_windows(&processes, previous, &mut candidates);
    adb::discover_generic_adb(
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
    let processes = process::query_processes();
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
    let mut mumu_roots: Vec<PathBuf> = mumu::discover_mumu_tool_installs(&processes)
        .into_iter()
        .map(|install| install.install_path)
        .collect();
    for process in processes
        .iter()
        .filter(|process| process.name.eq_ignore_ascii_case("MuMuVMMHeadless.exe"))
    {
        if let Some(exe) = process.executable_path.as_deref() {
            if let Some(root) = mumu::resolve_mumu_install_path(exe) {
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
    let mut ldplayer_roots: Vec<PathBuf> = ldplayer::discover_ldplayer_tool_installs(&processes);
    for process in processes
        .iter()
        .filter(|process| process.name.eq_ignore_ascii_case("Ld9BoxHeadless.exe"))
    {
        if let Some(exe) = process.executable_path.as_deref() {
            if let Some(root) = ldplayer::resolve_ldplayer_install_path(exe) {
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

#[allow(clippy::too_many_arguments)]
pub(super) fn base_config(
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
        replay_video_path: previous.and_then(|c| c.replay_video_path.clone()),
        replay_fps: previous.and_then(|c| c.replay_fps),
    }
}

pub(super) fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn stable_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
}
