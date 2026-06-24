//! Windows-window discovery: enumerate the visible top-level windows owned by
//! a running `Arknights.exe` and turn each into a window-capture candidate.

use std::path::Path;

use ruler_core::RulerConfig;
use ::windows::Win32::{
    Foundation::{BOOL, HWND, LPARAM},
    UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsWindowVisible,
    },
};

use super::{base_config, stable_path, ProcessInfo};
use crate::target_discovery::{LatencyClass, TargetCandidate, TargetKind};

#[derive(Clone, Debug)]
struct WindowInfo {
    hwnd: isize,
    title: String,
    class_name: String,
}

pub(super) fn discover_windows(
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
