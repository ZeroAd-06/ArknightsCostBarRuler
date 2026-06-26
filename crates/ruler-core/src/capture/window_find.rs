//! Shared Win32 window location for the PC capture backends.
//!
//! Both the GDI backend (`windows.rs`) and the Windows Graphics Capture backend
//! (`wgc.rs`) need to resolve a target `HWND` from an optional cached handle,
//! a window-title substring, and a window-class substring. This logic used to
//! be private to `WindowsController`; it lives here so both backends share one
//! definition instead of duplicating the lookup + `EnumWindows` scan.

use std::ffi::c_void;

use windows::Win32::Foundation::{BOOL, HWND, LPARAM};

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumWindows(
        lp_enum_func: Option<unsafe extern "system" fn(HWND, LPARAM) -> BOOL>,
        lparam: LPARAM,
    ) -> BOOL;
    fn FindWindowW(lp_class_name: *const u16, lp_window_name: *const u16) -> HWND;
    fn GetClassNameW(hwnd: HWND, lp_class_name: *mut u16, n_max_count: i32) -> i32;
    fn GetWindowTextLengthW(hwnd: HWND) -> i32;
    fn GetWindowTextW(hwnd: HWND, lp_string: *mut u16, n_max_count: i32) -> i32;
    fn IsWindow(hwnd: HWND) -> BOOL;
    fn IsWindowVisible(hwnd: HWND) -> BOOL;
}

struct SearchContext {
    title: Option<String>,
    class: Option<String>,
    found: Option<HWND>,
}

unsafe extern "system" fn enum_windows_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let context = unsafe { &mut *(lparam.0 as *mut SearchContext) };

    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return BOOL(1);
    }

    let title_len = unsafe { GetWindowTextLengthW(hwnd) };
    let title = if title_len > 0 {
        let mut buffer = vec![0u16; title_len as usize + 1];
        let read = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
        String::from_utf16_lossy(&buffer[..read as usize])
    } else {
        String::new()
    };

    let mut class_buffer = vec![0u16; 256];
    let class_len =
        unsafe { GetClassNameW(hwnd, class_buffer.as_mut_ptr(), class_buffer.len() as i32) };
    let class_name = String::from_utf16_lossy(&class_buffer[..class_len.max(0) as usize]);

    let title_match = context
        .title
        .as_ref()
        .map(|expected| title.contains(expected))
        .unwrap_or(true);
    let class_match = context
        .class
        .as_ref()
        .map(|expected| class_name.to_lowercase().contains(&expected.to_lowercase()))
        .unwrap_or(true);

    if title_match && class_match {
        context.found = Some(hwnd);
        BOOL(0)
    } else {
        BOOL(1)
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Resolve a target window handle from an optional cached raw handle, a
/// title substring, and a class-name substring.
///
/// Resolution order, mirroring the original `WindowsController::locate_window`:
/// 1. the cached `handle`, if it still names a live window;
/// 2. `FindWindowW` with the exact class+title;
/// 3. an `EnumWindows` scan matching the title/class as substrings.
pub(crate) fn locate_target_window(
    handle: Option<isize>,
    title: &Option<String>,
    class: &Option<String>,
) -> Result<HWND, String> {
    if let Some(value) = handle {
        let hwnd = HWND(value as *mut c_void);
        if unsafe { IsWindow(hwnd) }.as_bool() {
            return Ok(hwnd);
        }
    }

    if let Some(title) = title {
        let title_buf = wide_null(title);
        let class_buf = class.as_deref().map(wide_null);
        let hwnd = unsafe {
            FindWindowW(
                class_buf
                    .as_ref()
                    .map(|buf| buf.as_ptr())
                    .unwrap_or(std::ptr::null()),
                title_buf.as_ptr(),
            )
        };
        if !hwnd.0.is_null() {
            return Ok(hwnd);
        }
    }

    let mut context = SearchContext {
        title: title.clone(),
        class: class.clone(),
        found: None,
    };
    unsafe {
        let _ = EnumWindows(
            Some(enum_windows_proc),
            LPARAM((&mut context as *mut SearchContext) as isize),
        );
    }
    context
        .found
        .ok_or_else(|| "Could not find a matching target window".to_string())
}
