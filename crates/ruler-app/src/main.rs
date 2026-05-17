mod api;
mod app;
mod overlay;
mod tray;
mod worker;

use app::RulerApp;

fn main() {
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

fn run() -> Result<(), app::StartupError> {
    log::info!("bootstrapping ruler-app for Windows runtime");

    let app = RulerApp::build()?;
    app.run()
}
