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
    logging::LoggingRuntime,
    overlay::{OverlayPlacement, OverlayRuntime},
    resources::ResourceLocator,
    target_discovery::{discover_targets, probe_candidate_once},
    worker::{SharedAppState, StartupStatus, WorkerRuntime},
};

pub struct RulerApp {
    state: Arc<SharedAppState>,
    overlay: OverlayRuntime,
    api: ApiRuntime,
    worker: WorkerRuntime,
}

impl RulerApp {
    pub fn build(
        debug: bool,
        resources: ResourceLocator,
        logging: LoggingRuntime,
    ) -> Result<Self, StartupError> {
        let initial_status = determine_startup_status(&resources);
        let preferred_locale = initial_status
            .loaded_config
            .as_ref()
            .and_then(|config| config.language.as_deref());
        let i18n = Arc::new(I18n::load(&resources, preferred_locale));

        let startup_status = resolve_startup_config(&resources, &i18n, initial_status, debug)?;
        logging.apply_trace_setting(
            startup_status
                .loaded_config
                .as_ref()
                .map(|config| config.trace_logging_enabled)
                .unwrap_or(false),
        );

        let state = Arc::new(SharedAppState::default());
        state.update_startup_status(&startup_status);

        let icons = Arc::new(IconSet::load(&resources));
        let placement = startup_status
            .loaded_config
            .as_ref()
            .map(|config| OverlayPlacement {
                pos: match (config.overlay_pos_x, config.overlay_pos_y) {
                    (Some(x), Some(y)) => Some((x, y)),
                    _ => None,
                },
                scale_mult: config.overlay_scale.unwrap_or(1.0),
            })
            .unwrap_or_default();
        let (command_tx, command_rx) = mpsc::channel();
        let overlay = OverlayRuntime::new(
            Arc::clone(&state),
            command_tx.clone(),
            Arc::clone(&i18n),
            Arc::clone(&icons),
            placement,
        );
        let api = ApiRuntime::new(Arc::clone(&state));
        let worker = WorkerRuntime::spawn_from_startup(
            Arc::clone(&state),
            startup_status,
            resources,
            logging.session_dir(),
            command_rx,
            Duration::from_millis(1),
        )?;

        Ok(Self {
            state,
            overlay,
            api,
            worker,
        })
    }

    pub fn run(self) -> Result<(), StartupError> {
        let RulerApp {
            state,
            overlay,
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
        log::info!("startup plan: {}", api.startup_note());
        log::info!("startup plan: {}", worker.startup_note());

        #[cfg(windows)]
        {
            log::info!("Windows-oriented UI path selected");
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
    log::info!("loaded config from '{}'", config_path.display());
    let config = apply_runtime_ui_scaler(config);

    match config.to_capture_config() {
        Ok(_) => StartupStatus::ready(config_path_text, config),
        Err(error) => StartupStatus::invalid(config_path_text, error.to_string()),
    }
}

fn resolve_startup_config(
    resources: &ResourceLocator,
    i18n: &I18n,
    initial_status: StartupStatus,
    debug: bool,
) -> Result<StartupStatus, StartupError> {
    let config_path_text = resources.config_path().display().to_string();
    let previous_config = initial_status.loaded_config.clone();

    // --debug: force the config wizard regardless of current config.
    if debug {
        log::info!("debug mode: forcing config wizard");
        if let Some(config) = run_config_wizard(resources, i18n, previous_config.as_ref(), true) {
            config
                .save_to_path(resources.config_path())
                .map_err(|error| StartupError::new(error.to_string()))?;
            log::info!(
                "saved debug-wizard config to '{}'",
                resources.config_path().display()
            );
            return Ok(StartupStatus::ready(
                config_path_text,
                apply_runtime_ui_scaler(config),
            ));
        }
        return Ok(StartupStatus::invalid(
            config_path_text,
            "debug wizard cancelled".to_string(),
        ));
    }

    // Replay mode doesn't need target discovery or config wizard.
    if let Some(config) = previous_config.as_ref() {
        if config.capture_type == "replay" {
            return Ok(StartupStatus::ready(config_path_text, config.clone()));
        }
        if config.auto_select_target {
            match try_auto_select_config(config) {
                Ok(Some(config)) => {
                    config
                        .save_to_path(resources.config_path())
                        .map_err(|error| StartupError::new(error.to_string()))?;
                    log::info!(
                        "saved auto-selected config to '{}'",
                        resources.config_path().display()
                    );
                    return Ok(StartupStatus::ready(
                        config_path_text,
                        apply_runtime_ui_scaler(config),
                    ));
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

    if let Some(config) = run_config_wizard(resources, i18n, previous_config.as_ref(), false) {
        config
            .save_to_path(resources.config_path())
            .map_err(|error| StartupError::new(error.to_string()))?;
        log::info!(
            "saved config wizard selection to '{}'",
            resources.config_path().display()
        );
        return Ok(StartupStatus::ready(
            config_path_text,
            apply_runtime_ui_scaler(config),
        ));
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
    selected.overlay_pos_x = config.overlay_pos_x;
    selected.overlay_pos_y = config.overlay_pos_y;
    selected.overlay_scale = config.overlay_scale;
    selected.ui_scaler = config.ui_scaler;
    Ok(Some(selected))
}

fn apply_runtime_ui_scaler(mut config: RulerConfig) -> RulerConfig {
    if config.ui_scaler.is_some() || config.capture_type != "window" {
        return config;
    }

    match crate::arknights_settings::read_pc_ui_scaler() {
        Ok(Some(ui_scaler)) => {
            let ui_scaler = ui_scaler.clamp(0.0, 1.0);
            log::info!("detected Arknights PC uiScaler={ui_scaler:.3} from registry");
            config.ui_scaler = Some(ui_scaler);
        }
        Ok(None) => {
            log::info!("Arknights PC uiScaler registry value not found; using 1.0 layout");
        }
        Err(error) => {
            log::warn!("failed to read Arknights PC uiScaler: {error}; using 1.0 layout");
        }
    }

    config
}
