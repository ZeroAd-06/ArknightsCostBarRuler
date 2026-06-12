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
    target_discovery::{discover_targets, probe_candidate_once},
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
        let initial_status = determine_startup_status(&resources);
        let preferred_locale = initial_status
            .loaded_config
            .as_ref()
            .and_then(|config| config.language.as_deref());
        let i18n = Arc::new(I18n::load(&resources, preferred_locale));

        let startup_status = resolve_startup_config(&resources, &i18n, initial_status)?;

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

fn resolve_startup_config(
    resources: &ResourceLocator,
    i18n: &I18n,
    initial_status: StartupStatus,
) -> Result<StartupStatus, StartupError> {
    let config_path_text = resources.config_path().display().to_string();
    let previous_config = initial_status.loaded_config.clone();

    if let Some(config) = previous_config.as_ref() {
        if config.auto_select_target {
            match try_auto_select_config(config) {
                Ok(Some(config)) => {
                    config
                        .save_to_path(resources.config_path())
                        .map_err(|error| StartupError::new(error.to_string()))?;
                    return Ok(StartupStatus::ready(config_path_text, config));
                }
                Ok(None) => {
                    log::warn!("auto target selection did not find a usable matching target");
                }
                Err(error) => {
                    log::warn!("auto target selection failed: {error}");
                }
            }
        }
    }

    if let Some(config) = run_config_wizard(resources, i18n, previous_config.as_ref()) {
        config
            .save_to_path(resources.config_path())
            .map_err(|error| StartupError::new(error.to_string()))?;
        return Ok(StartupStatus::ready(config_path_text, config));
    }

    Ok(StartupStatus::invalid(
        config_path_text,
        format!(
            "{}: {}",
            initial_status.phase,
            i18n.tr("config.selector.cancelled")
        ),
    ))
}

fn try_auto_select_config(config: &RulerConfig) -> Result<Option<RulerConfig>, String> {
    let fingerprint = config.target_fingerprint.as_deref().ok_or_else(|| {
        "auto_select_target is true but target_fingerprint is missing".to_string()
    })?;
    let candidates = discover_targets(Some(config));
    let Some(candidate) = candidates
        .into_iter()
        .find(|candidate| candidate.fingerprint == fingerprint)
    else {
        return Ok(None);
    };
    let probe = probe_candidate_once(&candidate);
    if let Some(error) = probe.error {
        return Err(error);
    }
    if probe.preview.is_none() {
        return Err("matching target did not produce a screenshot preview".to_string());
    }
    if let Some(latency) = probe.latency {
        log::info!(
            "auto target screenshot probe ok: fingerprint={}, average_seed_ms={:.1}, class={:?}",
            probe.fingerprint,
            latency.as_secs_f64() * 1000.0,
            probe.latency_class
        );
    }

    let mut selected = candidate.config;
    selected.auto_select_target = true;
    selected.target_fingerprint = Some(fingerprint.to_string());
    selected.active_calibration_profile = config.active_calibration_profile.clone();
    selected.frame_display_mode = config.frame_display_mode.clone();
    selected.language = config.language.clone();
    Ok(Some(selected))
}
