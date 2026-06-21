#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod api;
mod app;
mod arknights_settings;
mod commands;
mod config_wizard;
mod debug_recorder;
mod fonts;
mod i18n;
mod icons;
mod logging;
mod menu;
mod overlay;
mod profiles;
mod resources;
#[cfg(windows)]
mod pc_cursor_guard;
#[cfg(windows)]
mod slint_win;
mod target_discovery;
mod tray;
mod ui;
mod ui_state;
mod worker;

use app::RulerApp;
use logging::LoggingRuntime;
use resources::ResourceLocator;

pub(crate) const ICU_PROVIDER_ERROR_LOG_TARGET: &str = "icu_provider::error";

fn main() {
    enable_dpi_awareness();
    let debug = std::env::args().any(|a| a == "--debug" || a == "-d");
    if relaunch_as_admin_if_needed() {
        return;
    }
    let resources = ResourceLocator::new();
    let logging = match LoggingRuntime::init(&resources) {
        Ok(logging) => logging,
        Err(error) => {
            eprintln!("failed to initialize file logging: {error}");
            std::process::exit(1);
        }
    };

    if let Err(error) = run(debug, resources, logging) {
        log::error!("ruler-app failed to start: {error}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn enable_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[cfg(not(windows))]
fn enable_dpi_awareness() {}

#[cfg(windows)]
fn relaunch_as_admin_if_needed() -> bool {
    use windows::Win32::Foundation::{BOOL, HINSTANCE, HWND};

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn IsUserAnAdmin() -> BOOL;
        fn ShellExecuteW(
            hwnd: HWND,
            lpoperation: *const u16,
            lpfile: *const u16,
            lpparameters: *const u16,
            lpdirectory: *const u16,
            nshowcmd: i32,
        ) -> HINSTANCE;
    }

    unsafe {
        if IsUserAnAdmin().as_bool() {
            return false;
        }
    }

    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let exe_wide = wide_os(exe.as_os_str());
    let args = std::env::args_os()
        .skip(1)
        .map(|arg| quote_arg(&arg.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    let args_wide = wide(&args);
    let runas = wide("runas");
    let result = unsafe {
        ShellExecuteW(
            HWND::default(),
            runas.as_ptr(),
            exe_wide.as_ptr(),
            if args.is_empty() {
                std::ptr::null()
            } else {
                args_wide.as_ptr()
            },
            std::ptr::null(),
            1,
        )
    };
    let code = result.0 as isize;
    if code > 32 {
        true
    } else {
        eprintln!("failed to relaunch as administrator: ShellExecuteW returned {code}");
        true
    }
}

#[cfg(not(windows))]
fn relaunch_as_admin_if_needed() -> bool {
    false
}

#[cfg(windows)]
fn quote_arg(value: &str) -> String {
    if value.is_empty() || value.chars().any(|ch| ch.is_whitespace() || ch == '"') {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn wide_os(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn run(
    debug: bool,
    resources: ResourceLocator,
    logging: LoggingRuntime,
) -> Result<(), app::StartupError> {
    log::info!("bootstrapping ruler-app for Windows runtime (debug={debug})");

    let app = RulerApp::build(debug, resources, logging)?;
    app.run()
}
