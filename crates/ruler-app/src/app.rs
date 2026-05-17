use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};

use ruler_core::RulerConfig;

use crate::{
    api::ApiRuntime,
    overlay::{OverlayRuntime, OverlaySpec},
    tray::TrayRuntime,
    worker::{SharedAppState, StartupStatus, WorkerRuntime},
};

pub struct RulerApp {
    state: Arc<SharedAppState>,
    overlay: OverlayRuntime,
    tray: TrayRuntime,
    api: ApiRuntime,
    worker: WorkerRuntime,
}

impl RulerApp {
    pub fn build() -> Result<Self, StartupError> {
        let state = Arc::new(SharedAppState::default());
        let startup_status = determine_startup_status();
        state.update_startup_status(&startup_status);
        let overlay = OverlayRuntime::new(OverlaySpec::default(), Arc::clone(&state));
        let tray = TrayRuntime::new(Arc::clone(&state));
        let api = ApiRuntime::new(Arc::clone(&state));
        let worker = WorkerRuntime::spawn_from_startup(
            Arc::clone(&state),
            startup_status,
            Duration::from_millis(1),
        )?;

        Ok(Self {
            state,
            overlay,
            tray,
            api,
            worker,
        })
    }

    pub fn run(self) -> Result<(), StartupError> {
        let RulerApp {
            state,
            overlay,
            tray,
            api,
            worker,
        } = self;

        let startup_snapshot = state.snapshot();
        log::info!(
            "ruler-app state pipeline ready with {} / {} / {} / {}",
            startup_snapshot.status_text,
            startup_snapshot.frame_text,
            startup_snapshot.timer_text,
            startup_snapshot.latency_text
        );
        log::info!("startup plan: {}", overlay.startup_note());
        log::info!("startup plan: {}", tray.startup_note());
        log::info!("startup plan: {}", api.startup_note());
        log::info!("startup plan: {}", worker.startup_note());

        for tick in 1..=2 {
            thread::sleep(Duration::from_millis(350));
            let snapshot = state.snapshot();
            log::info!(
                "state tick {tick}: {} / {} / {} / {}",
                snapshot.status_text,
                snapshot.frame_text,
                snapshot.timer_text,
                snapshot.latency_text
            );
        }

        #[cfg(windows)]
        {
            log::info!("Windows-oriented bootstrap path selected");
            let _tray = tray.run().map_err(StartupError::from)?;
            let result = overlay.run().map_err(StartupError::from);
            drop(worker);
            result
        }

        #[cfg(not(windows))]
        {
            log::warn!("Non-Windows host detected; native overlay bootstrap is unavailable");
            drop(worker);
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct StartupError {
    message: String,
}

impl StartupError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StartupError {}

impl From<crate::overlay::OverlayError> for StartupError {
    fn from(value: crate::overlay::OverlayError) -> Self {
        Self::new(value.to_string())
    }
}

impl From<crate::tray::TrayError> for StartupError {
    fn from(value: crate::tray::TrayError) -> Self {
        Self::new(value.to_string())
    }
}

fn determine_startup_status() -> StartupStatus {
    let config_path = default_config_path();
    let config_path_text = config_path.display().to_string();

    if !config_path.exists() {
        return StartupStatus::missing(config_path_text);
    }

    let config = match RulerConfig::load_from_path(&config_path) {
        Ok(config) => config,
        Err(error) => return StartupStatus::invalid(config_path_text, error.to_string()),
    };

    match config.to_capture_config() {
        Ok(_) => StartupStatus::engine_not_connected(
            config_path_text,
            config.clone(),
            format!(
                "config loaded for '{}' capture; runtime has not connected the engine yet; active_calibration_profile={}",
                config.capture_type,
                config.active_calibration_profile.as_deref().unwrap_or("--")
            ),
        ),
        Err(error) => StartupStatus::invalid(config_path.display().to_string(), error.to_string()),
    }
}

fn default_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("config.json")
}
