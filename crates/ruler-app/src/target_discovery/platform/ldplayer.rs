//! LDPlayer emulator discovery: enumerate instances via `dnconsole list2`, and
//! fall back to inspecting running `Ld9BoxHeadless.exe` processes (mapping them
//! back to an instance index and its VBox NAT listener ports) when needed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ruler_core::RulerConfig;

use super::adb::{
    adb_has_arknights_package, adb_serial_for_ports, ldplayer_adb_serial_for_instance,
};
use super::process::{matching_vbox_pids, related_listener_ports, run_dnconsole_text};
use super::{
    base_config, non_empty, stable_path, ProcessInfo, TcpListener, COMMAND_TIMEOUT,
};
use crate::target_discovery::{LatencyClass, TargetCandidate, TargetKind};

#[derive(Clone, Debug)]
struct LDPlayerInstance {
    index: u32,
    name: String,
    player_pid: u32,
    vbox_pid: u32,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn discover_ldplayer(
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

pub(super) fn discover_ldplayer_tool_installs(processes: &[ProcessInfo]) -> Vec<PathBuf> {
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

pub(super) fn resolve_ldplayer_install_path(executable_path: &str) -> Option<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::super::test_support::{process, unique_test_dir};
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
}
