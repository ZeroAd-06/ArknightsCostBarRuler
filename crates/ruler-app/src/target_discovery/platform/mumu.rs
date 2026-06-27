//! MuMu emulator discovery: scan for `MuMuManager.exe`, parse its JSON instance
//! listing, and fall back to inspecting running `MuMuVMMHeadless.exe` processes
//! when the manager tool isn't reachable.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ruler_core::RulerConfig;
use serde_json::Value;

use super::adb::{
    adb_has_arknights_package, adb_serial_for_address, adb_serial_for_ports, normalize_adb_host,
};
use super::process::{
    parse_named_number, parse_trailing_segment_number, related_listener_ports, run_command_text,
};
use super::{base_config, non_empty, stable_path, ProcessInfo, TcpListener, COMMAND_TIMEOUT};
use crate::target_discovery::{LatencyClass, TargetCandidate, TargetKind};

#[derive(Clone, Debug)]
pub(super) struct MuMuInstall {
    pub(super) install_path: PathBuf,
    manager_path: PathBuf,
}

#[derive(Clone, Debug)]
struct MuMuManagerInfo {
    index: u32,
    name: Option<String>,
    host: String,
    port: u16,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn discover_mumu(
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
        let Ok(output) = run_command_text(&program, &["info", "--vmindex", "all"], COMMAND_TIMEOUT)
        else {
            continue;
        };

        for info in parse_mumu_manager_infos(&output) {
            let Some(serial) = adb_serial_for_address(&info.host, info.port, connected_adb_serials)
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

pub(super) fn discover_mumu_tool_installs(processes: &[ProcessInfo]) -> Vec<MuMuInstall> {
    let mut seen = HashSet::new();
    let mut installs = Vec::new();
    for process in processes
        .iter()
        .filter(|process| is_mumu_discovery_process(&process.name))
    {
        let Some(install_path) =
            resolve_mumu_install_path_for_process(process, processes).or_else(|| {
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

pub(super) fn resolve_mumu_install_path(executable_path: &str) -> Option<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::super::test_support::{process, unique_test_dir};
    use super::*;

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
        let output = r#"{"index":"2","name":"MuMu-2","adb_host_ip":"0.0.0.0","adb_port":16448}"#;
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
            resolve_mumu_install_path_for_process(&headless, &[headless.clone(), device]).unwrap();

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
}
