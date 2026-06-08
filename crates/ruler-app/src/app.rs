use std::{
    fmt,
    sync::{mpsc, Arc},
    time::Duration,
};

use ruler_core::RulerConfig;

use crate::{
    api::ApiRuntime,
    config_wizard::run_config_wizard,
    i18n::I18n,
    icons::IconSet,
    overlay::OverlayRuntime,
    resources::ResourceLocator,
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
        let resources = ResourceLocator::new();
        let mut startup_status = determine_startup_status(&resources);
        let preferred_locale = startup_status
            .loaded_config
            .as_ref()
            .and_then(|config| config.language.as_deref());
        let i18n = Arc::new(I18n::load(&resources, preferred_locale));

        if startup_status.loaded_config.is_none() {
            if let Some(config) = run_config_wizard(&resources, &i18n) {
                config
                    .save_to_path(resources.config_path())
                    .map_err(|error| StartupError::new(error.to_string()))?;
                startup_status =
                    StartupStatus::ready(resources.config_path().display().to_string(), config);
            }
        }

        let state = Arc::new(SharedAppState::default());
        state.update_startup_status(&startup_status);

        let icons = Arc::new(IconSet::load(&resources));
        let (command_tx, command_rx) = mpsc::channel();
        let overlay = OverlayRuntime::new(
            Arc::clone(&state),
            command_tx.clone(),
            Arc::clone(&i18n),
            Arc::clone(&icons),
        );
        let tray = TrayRuntime::new(
            Arc::clone(&state),
            command_tx.clone(),
            Arc::clone(&i18n),
            Arc::clone(&icons),
        );
        let api = ApiRuntime::new(Arc::clone(&state));
        let worker = WorkerRuntime::spawn_from_startup(
            Arc::clone(&state),
            startup_status,
            resources,
            command_rx,
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
            "ruler-app UI pipeline ready: mode={:?}, message={}",
            startup_snapshot.ui.mode,
            startup_snapshot.ui.message
        );
        log::info!("startup plan: {}", overlay.startup_note());
        log::info!("startup plan: {}", tray.startup_note());
        log::info!("startup plan: {}", api.startup_note());
        log::info!("startup plan: {}", worker.startup_note());

        #[cfg(windows)]
        {
            log::info!("Windows-oriented UI path selected");
            let _tray = tray.run().map_err(StartupError::from)?;
            let result = overlay.run().map_err(StartupError::from);
            drop(worker);
            drop(api);
            result
        }

        #[cfg(not(windows))]
        {
            log::warn!("Non-Windows host detected; native overlay is unavailable");
            drop(worker);
            drop(api);
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

fn determine_startup_status(resources: &ResourceLocator) -> StartupStatus {
    let config_path = resources.config_path();
    let config_path_text = config_path.display().to_string();

    if !config_path.exists() {
        return StartupStatus::missing(config_path_text);
    }

    let config = match RulerConfig::load_from_path(&config_path) {
        Ok(config) => config,
        Err(error) => return StartupStatus::invalid(config_path_text, error.to_string()),
    };

    match config.to_capture_config() {
        Ok(_) => StartupStatus::ready(config_path_text, config),
        Err(error) => StartupStatus::invalid(config_path.display().to_string(), error.to_string()),
    }
}
