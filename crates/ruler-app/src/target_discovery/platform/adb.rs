//! ADB serial handling: enumerate connected serials, resolve a serial for a
//! host:port (connecting on demand), check for an installed Arknights package,
//! and emit generic-ADB candidates for serials not claimed by an emulator.

use std::collections::HashSet;

use ruler_core::RulerConfig;

use super::process::run_adb_text;
use super::{base_config, COMMAND_TIMEOUT};
use crate::target_discovery::{LatencyClass, TargetCandidate, TargetKind};

const PACKAGE_NAMES: &[&str] = &[
    "com.hypergryph.arknights",
    "com.hypergryph.arknights.bilibili",
    "tw.txwy.and.arknights",
    "com.YoStarEN.Arknights",
    "com.YoStarJP.Arknights",
    "com.YoStarKR.Arknights",
];

pub(super) fn discover_generic_adb(
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
        if serial_port(serial).is_some_and(|port| claimed_ports.contains(&port)) {
            continue;
        }
        if adb_has_arknights_package(serial) && seen_serials.insert(serial.clone()) {
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

pub(super) fn adb_serial_for_ports<'a>(
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

pub(super) fn adb_serial_for_address(
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

pub(super) fn normalize_adb_host(host: &str) -> String {
    match host.trim() {
        "" | "0.0.0.0" | "localhost" => "127.0.0.1".to_string(),
        host => host.to_string(),
    }
}

pub(super) fn ldplayer_adb_serial_for_instance(
    instance_index: u32,
    connected_adb_serials: &[String],
) -> Option<String> {
    let serial = format!("emulator-{}", 5554 + instance_index * 2);
    connected_adb_serials
        .iter()
        .any(|connected| connected == &serial)
        .then_some(serial)
}

pub(super) fn adb_connected_serials() -> Vec<String> {
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

pub(super) fn adb_has_arknights_package(serial: &str) -> bool {
    PACKAGE_NAMES.iter().any(|package| {
        run_adb_text(
            &["-s", serial, "shell", "pm", "path", package],
            COMMAND_TIMEOUT,
        )
        .map(|output| output.contains("package:"))
        .unwrap_or(false)
    })
}
