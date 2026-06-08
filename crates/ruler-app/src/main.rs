#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod api;
mod app;
mod commands;
mod config_wizard;
mod i18n;
mod icons;
mod menu;
mod overlay;
mod profiles;
mod resources;
mod tray;
mod ui_state;
mod worker;

use app::RulerApp;

fn main() {
    enable_dpi_awareness();
    init_logging();

    if let Err(error) = run() {
        log::error!("ruler-app failed to start: {error}");
        std::process::exit(1);
    }
}

fn init_logging() {
    let env = env_logger::Env::default().filter_or("RUST_LOG", "info");
    env_logger::Builder::from_env(env)
        .format_timestamp_millis()
        .init();
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

fn run() -> Result<(), app::StartupError> {
    log::info!("bootstrapping ruler-app for Windows runtime");

    let app = RulerApp::build()?;
    app.run()
}
