//! System-level queries shared by every backend: the process / TCP-listener
//! snapshot, hidden-window command execution (adb, MuMuManager, dnconsole),
//! ANSI decoding, generic number parsing, and process-relationship helpers.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use netstat2::{get_sockets_info, AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState};
use ruler_core::capture::adb_resolver::adb_command;
use sysinfo::System;
use windows::Win32::Globalization::{MultiByteToWideChar, CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};

use super::{ProcessInfo, TcpListener};

pub(super) fn query_processes() -> Vec<ProcessInfo> {
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

pub(super) fn query_tcp_listeners() -> Vec<TcpListener> {
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

pub(super) fn matching_vbox_pids(
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

pub(super) fn related_listener_ports(
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

fn ancestors(process: &ProcessInfo, process_by_pid: &HashMap<u32, ProcessInfo>) -> HashSet<u32> {
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

pub(super) fn run_command_text(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    log::trace!("target discovery command: {} {}", program, args.join(" "));
    let mut command = Command::new(program);
    configure_hidden_command(&mut command);
    let bytes = run_command_capture(&mut command, program, args, timeout)?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

/// Same as `run_command_text` but uses the resolved adb executable
/// (PATH or emulator-bundled) instead of a literal `"adb"` lookup.
/// adb/MuMu output is UTF-8, so decode as such.
pub(super) fn run_adb_text(args: &[&str], timeout: Duration) -> Result<String, String> {
    log::trace!("target discovery adb command: adb {}", args.join(" "));
    let mut command = adb_command()?;
    let bytes = run_command_capture(&mut command, "adb", args, timeout)?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

/// Run `dnconsole.exe` and decode its output as the system ANSI code page
/// (GBK / CP 936 on zh_CN). `dnconsole list2` writes GBK bytes, so a naive
/// UTF-8 decode turns Chinese instance names into replacement characters.
/// MuMu's `MuMuManager.exe` output is UTF-8 and must stay on `run_command_text`.
pub(super) fn run_dnconsole_text(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    log::trace!(
        "target discovery dnconsole command: {} {}",
        program,
        args.join(" ")
    );
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
    let wide_len =
        unsafe { MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None) };
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
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

pub(super) fn parse_named_number(text: &str, names: &[&str]) -> Option<u32> {
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

pub(super) fn parse_trailing_segment_number(text: &str, marker: &str) -> Option<u32> {
    let lower = text.to_ascii_lowercase();
    let marker = marker.to_ascii_lowercase();
    let index = lower.rfind(&marker)?;
    first_number_after(&lower[index + marker.len()..])
}

#[cfg(test)]
mod tests {
    use super::super::test_support::process;
    use super::*;

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
}
