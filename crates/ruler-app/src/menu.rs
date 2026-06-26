//! The "about" page launcher.
//!
//! The right-click menu and all modal dialogs (rename / delete / errors) are now
//! Slint popups rendered by `overlay.rs`; the only native shell call left here is
//! opening the project page in the default browser.

#[cfg(windows)]
pub mod win32 {
    use std::iter;

    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::HWND,
            UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        },
    };

    const ABOUT_PAGE_URL: &str = "https://github.com/ZeroAd-06/ArknightsCostBarRuler";

    pub unsafe fn open_about_page() {
        open_url(ABOUT_PAGE_URL);
    }

    pub unsafe fn open_url(url: &str) {
        let operation = wide("open");
        let url = wide(url);
        let _ = ShellExecuteW(
            HWND::default(),
            PCWSTR(operation.as_ptr()),
            PCWSTR(url.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(iter::once(0)).collect()
    }
}
