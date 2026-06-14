//! System-tray icon helpers.
//!
//! The tray icon is owned by the overlay's window on the UI thread (Slint
//! windows are `!Send`, so the right-click menu must be built on the same
//! thread that owns the Slint platform). This module only provides the
//! `Shell_NotifyIcon` plumbing; the overlay drives it.

#[cfg(windows)]
pub mod win32 {
    use std::mem;

    use crate::icons::{win32::create_icon, IconSet};
    use windows::Win32::{
        Foundation::{HINSTANCE, HWND},
        UI::{
            Shell::{
                Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
                NOTIFYICONDATAW,
            },
            WindowsAndMessaging::{DestroyIcon, LoadIconW, HICON, IDI_APPLICATION},
        },
    };

    /// Default tooltip (app name). Status text replaces it on demand.
    const DEFAULT_TIP: &str = "明日方舟费用条尺子";

    /// Register a tray icon bound to `hwnd`, delivering `callback_msg` on
    /// interaction. Returns the notify-icon data (needed for later modify/delete)
    /// and the custom HICON that must be destroyed on teardown, or `None` on
    /// failure.
    pub unsafe fn install(
        hwnd: HWND,
        icons: &IconSet,
        callback_msg: u32,
    ) -> Option<(NOTIFYICONDATAW, Option<HICON>)> {
        let custom_icon = icons.get("deco").and_then(|image| create_icon(image, 32));
        let icon = match custom_icon {
            Some(icon) => icon,
            None => LoadIconW(HINSTANCE::default(), IDI_APPLICATION).ok()?,
        };

        let nid = NOTIFYICONDATAW {
            cbSize: mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: callback_msg,
            hIcon: icon,
            szTip: tooltip_text(DEFAULT_TIP),
            ..Default::default()
        };

        if Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            Some((nid, custom_icon))
        } else {
            if let Some(icon) = custom_icon {
                let _ = DestroyIcon(icon);
            }
            None
        }
    }

    /// Update the hover tooltip text.
    pub unsafe fn update_tooltip(nid: &mut NOTIFYICONDATAW, text: &str) {
        nid.szTip = tooltip_text(text);
        let _ = Shell_NotifyIconW(NIM_MODIFY, nid);
    }

    /// Remove the tray icon and free the custom HICON.
    pub unsafe fn remove(nid: &NOTIFYICONDATAW, custom_icon: Option<HICON>) {
        let _ = Shell_NotifyIconW(NIM_DELETE, nid);
        if let Some(icon) = custom_icon {
            let _ = DestroyIcon(icon);
        }
    }

    fn tooltip_text(status: &str) -> [u16; 128] {
        let text = truncate_menu_text(status);
        let mut buf = [0u16; 128];
        for (index, value) in text.encode_utf16().take(buf.len() - 1).enumerate() {
            buf[index] = value;
        }
        buf
    }

    fn truncate_menu_text(value: &str) -> String {
        const LIMIT: usize = 64;
        if value.chars().count() <= LIMIT {
            value.to_string()
        } else {
            let shortened: String = value.chars().take(LIMIT - 1).collect();
            format!("{shortened}…")
        }
    }
}
