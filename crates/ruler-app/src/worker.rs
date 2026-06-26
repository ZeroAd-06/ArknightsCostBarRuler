use std::{
    collections::VecDeque,
    fmt,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, TryRecvError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ruler_core::RulerConfig;

use crate::{
    commands::UiCommand,
    profiles::ProfileStore,
    resources::ResourceLocator,
    ui_state::{
        ApiFrameLookup, ApiFrameRecord, ApiHistoryBounds, ApiStateSnapshot, UiSnapshot,
        UpdateNotice,
    },
};

#[derive(Clone, Copy, Debug, Default)]
pub struct WorkerTimingSnapshot {
    pub sample_index: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OverlayTimingSnapshot {
    pub last_painted_sample_index: Option<u64>,
    pub last_painted_at: Option<Instant>,
}

#[derive(Clone, Debug, Default)]
pub struct AppStateSnapshot {
    pub ui: UiSnapshot,
    pub api: ApiStateSnapshot,
    pub worker_timing: WorkerTimingSnapshot,
    pub overlay_timing: OverlayTimingSnapshot,
}

#[derive(Default)]
pub struct SharedAppState {
    inner: Mutex<AppStateSnapshot>,
    api_history: Mutex<VecDeque<ApiFrameRecord>>,
    overlay_waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    // One-shot flag set by the UI to abort an in-flight calibration loop.
    // The worker polls it inside `collect_calibration_samples` and bails out.
    cancel_calibration: AtomicBool,
}

impl fmt::Debug for SharedAppState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedAppState")
            .field("snapshot", &self.snapshot())
            .finish_non_exhaustive()
    }
}

impl SharedAppState {
    #[must_use]
    pub fn snapshot(&self) -> AppStateSnapshot {
        self.inner
            .lock()
            .expect("shared app state poisoned")
            .clone()
    }

    #[must_use]
    pub fn api_history_bounds(&self) -> ApiHistoryBounds {
        let history = self.api_history.lock().expect("shared app state poisoned");
        ApiHistoryBounds {
            oldest_frame_id: history.front().map(|record| record.frame_id),
            latest_frame_id: history.back().map(|record| record.frame_id),
        }
    }

    pub fn record_api_frame(&self, record: ApiFrameRecord) {
        let mut history = self.api_history.lock().expect("shared app state poisoned");
        history.push_back(record);
    }

    pub fn clear_api_frame_history(&self) {
        let mut history = self.api_history.lock().expect("shared app state poisoned");
        history.clear();
    }

    #[must_use]
    pub fn api_frame_at_or_before(&self, requested_frame_id: u64) -> ApiFrameLookup {
        let history = self.api_history.lock().expect("shared app state poisoned");
        let Some(record) = history
            .iter()
            .rev()
            .find(|record| record.frame_id <= requested_frame_id)
            .cloned()
        else {
            return ApiFrameLookup::NotRetained { requested_frame_id };
        };

        let fell_back = record.frame_id != requested_frame_id;
        let fallback_reason = if fell_back {
            let latest_frame_id = history.back().map(|latest| latest.frame_id);
            Some(
                if latest_frame_id.is_some_and(|latest| requested_frame_id > latest) {
                    "requested_after_latest"
                } else {
                    "frame_skipped"
                }
                .to_string(),
            )
        } else {
            None
        };

        ApiFrameLookup::Found {
            requested_frame_id,
            record,
            fell_back,
            fallback_reason,
        }
    }

    pub fn update_startup_status(&self, startup: &StartupStatus) {
        {
            let mut state = self.inner.lock().expect("shared app state poisoned");
            state.ui.mode = if startup.loaded_config.is_some() {
                OverlayMode::Booting
            } else {
                OverlayMode::Error
            };
            state.ui.message = format!(
                "{}: {} ({})",
                startup.phase, startup.detail, startup.config_path
            );
            state.ui.display_mode = startup
                .loaded_config
                .as_ref()
                .map(|config| FrameDisplayMode::from_config(config.frame_display_mode.as_deref()))
                .unwrap_or_default();
            state.api.active_profile = startup.loaded_config.as_ref().and_then(|config| {
                config
                    .active_calibration_profile
                    .as_deref()
                    .map(calibration_basename)
            });
            state.ui.overlay_scale_pct = startup
                .loaded_config
                .as_ref()
                .and_then(|config| config.overlay_scale)
                .map(|mult| (mult * 100.0).round().clamp(50.0, 400.0) as u16)
                .unwrap_or(100);
            state.ui.cursor_blocked = false;
        }
        self.notify_overlay();
    }

    pub fn update_ui(&self, updater: impl FnOnce(&mut UiSnapshot, &mut ApiStateSnapshot)) {
        {
            let mut state = self.inner.lock().expect("shared app state poisoned");
            let AppStateSnapshot { ui, api, .. } = &mut *state;
            updater(ui, api);
        }
        self.notify_overlay();
    }

    pub fn update_timing(&self, worker_timing: WorkerTimingSnapshot) {
        {
            let mut state = self.inner.lock().expect("shared app state poisoned");
            state.worker_timing = worker_timing;
        }
        self.notify_overlay();
    }

    pub fn request_exit(&self) {
        self.update_ui(|ui, _| ui.should_exit = true);
    }

    pub fn set_update_notice(&self, notice: Option<UpdateNotice>) {
        self.update_ui(|ui, _| ui.update_notice = notice);
    }

    /// Set the one-shot cancel flag for an in-flight calibration. The worker
    /// polls this inside the capture loop and aborts promptly. Has no effect if
    /// no calibration is running — the flag is cleared at the start of the next
    /// `run_calibration` call.
    pub fn request_cancel_calibration(&self) {
        self.cancel_calibration.store(true, Ordering::SeqCst);
    }

    /// Atomically read and clear the cancel flag. Returns `true` if a cancel
    /// was requested since the last call.
    pub fn take_cancel_calibration(&self) -> bool {
        self.cancel_calibration.swap(false, Ordering::SeqCst)
    }

    pub fn record_overlay_paint(&self, sample_index: u64, painted_at: Instant) {
        let mut state = self.inner.lock().expect("shared app state poisoned");
        state.overlay_timing.last_painted_sample_index = Some(sample_index);
        state.overlay_timing.last_painted_at = Some(painted_at);
    }

    pub fn set_overlay_waker(&self, waker: Option<Arc<dyn Fn() + Send + Sync>>) {
        let mut overlay_waker = self
            .overlay_waker
            .lock()
            .expect("shared app state poisoned");
        *overlay_waker = waker;
    }

    fn notify_overlay(&self) {
        let overlay_waker = self
            .overlay_waker
            .lock()
            .expect("shared app state poisoned")
            .clone();

        if let Some(waker) = overlay_waker {
            waker();
        }
    }
}

#[derive(Clone, Debug)]
pub struct StartupStatus {
    pub phase: String,
    pub detail: String,
    pub config_path: String,
    pub loaded_config: Option<RulerConfig>,
}

impl StartupStatus {
    #[must_use]
    pub fn missing(config_path: String) -> Self {
        Self {
            phase: "config missing".to_string(),
            detail: "config.json was not found".to_string(),
            config_path,
            loaded_config: None,
        }
    }

    #[must_use]
    pub fn invalid(config_path: String, detail: String) -> Self {
        Self {
            phase: "config invalid".to_string(),
            detail,
            config_path,
            loaded_config: None,
        }
    }

    #[must_use]
    pub fn ready(config_path: String, loaded_config: RulerConfig) -> Self {
        Self {
            phase: "config loaded".to_string(),
            detail: format!(
                "config loaded for '{}' capture; active_calibration_profile={}",
                loaded_config.capture_type,
                loaded_config
                    .active_calibration_profile
                    .as_deref()
                    .unwrap_or("--")
            ),
            config_path,
            loaded_config: Some(loaded_config),
        }
    }
}

#[derive(Debug)]
pub struct WorkerRuntime {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl WorkerRuntime {
    pub fn spawn_from_startup(
        state: Arc<SharedAppState>,
        startup: StartupStatus,
        resources: ResourceLocator,
        log_session_dir: PathBuf,
        commands: Receiver<UiCommand>,
        interval: Duration,
    ) -> Result<Self, crate::app::StartupError> {
        let running = Arc::new(AtomicBool::new(true));
        let worker_running = Arc::clone(&running);
        let handle = thread::Builder::new()
            .name("ruler-app-worker".to_string())
            .spawn(move || {
                run_worker_loop(
                    state,
                    startup,
                    resources,
                    log_session_dir,
                    commands,
                    worker_running,
                    interval,
                );
            })
            .map_err(|error| {
                crate::app::StartupError::new(format!("failed to start worker thread: {error}"))
            })?;

        Ok(Self {
            running,
            handle: Some(handle),
        })
    }

    #[must_use]
    pub fn startup_note(&self) -> &'static str {
        "background worker is active and accepts UI commands"
    }
}

impl Drop for WorkerRuntime {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);

        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Worker context + main loop (three-layer architecture)
// ---------------------------------------------------------------------------

use std::sync::mpsc::Sender;

use ruler_core::{
    analysis::roi,
    pipeline::{PipelineConfig, PipelineInfo},
    CapturePipeline,
};

use crate::{
    analyzer_consumer::{AnalyzerCommand, AnalyzerConfig, AnalyzerConsumer},
    calibration,
    debug_recorder::{DebugRecorderConfig, DebugRecorderConsumer},
    profiles::calibration_basename,
    telemetry::{self, RunTelemetryStats},
    ui_state::{FrameDisplayMode, OverlayMode},
};

/// The worker context in the three-layer architecture. The worker owns the
/// Layer 1 pipeline and the Layer 2 analyzer consumer, and is responsible
/// for command dispatch and lifecycle management.
struct WorkerContext {
    config: RulerConfig,
    config_path: PathBuf,
    profiles: ProfileStore,
    active_profile: Option<String>,
    display_mode: FrameDisplayMode,
    log_session_dir: PathBuf,
    session_id: String,
    pipeline: Option<CapturePipeline>,
    pipeline_info: Option<PipelineInfo>,
    analyzer_command_tx: Option<Sender<AnalyzerCommand>>,
    analyzer: Option<AnalyzerConsumer>,
    debug_recorder: Option<DebugRecorderConsumer>,
    telemetry_stats: Arc<RunTelemetryStats>,
}

fn run_worker_loop(
    state: Arc<SharedAppState>,
    startup: StartupStatus,
    resources: ResourceLocator,
    log_session_dir: PathBuf,
    commands: Receiver<UiCommand>,
    running: Arc<AtomicBool>,
    interval: Duration,
) {
    state.update_startup_status(&startup);

    let Some(config) = startup.loaded_config.clone() else {
        wait_for_exit_commands(&state, &commands, &running, interval);
        return;
    };

    let profiles = ProfileStore::new(&resources);
    let session_id = format!(
        "{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );

    let mut context = WorkerContext {
        display_mode: FrameDisplayMode::from_config(config.frame_display_mode.as_deref()),
        active_profile: config.active_calibration_profile.clone(),
        config,
        config_path: resources.config_path(),
        profiles,
        log_session_dir,
        session_id,
        pipeline: None,
        pipeline_info: None,
        analyzer_command_tx: None,
        analyzer: None,
        debug_recorder: None,
        telemetry_stats: Arc::new(RunTelemetryStats::default()),
    };

    if let Err(error) = bootstrap(&mut context, Arc::clone(&state)) {
        publish_error(&state, &context, error);
    }

    let mut next_poll_at = Instant::now();
    while running.load(Ordering::Relaxed) {
        if !drain_commands(&state, &mut context, &commands, &running) {
            break;
        }
        next_poll_at = next_poll_at.max(Instant::now()) + interval;
        sleep_until(next_poll_at, &running);
    }

    // Shutdown: stop the analyzer first, then the pipeline.
    if let Some(tx) = context.analyzer_command_tx.take() {
        let _ = tx.send(AnalyzerCommand::Shutdown);
    }
    drop(context.analyzer.take());
    drop(context.debug_recorder.take());
    if let Some(mut pipeline) = context.pipeline.take() {
        pipeline.shutdown();
    }
    if let Err(error) =
        telemetry::write_session_stats(&context.log_session_dir, &context.telemetry_stats)
    {
        log::warn!("{error}");
    }
}

fn bootstrap(context: &mut WorkerContext, state: Arc<SharedAppState>) -> Result<(), String> {
    // Read ui_scaler (same logic as the old bootstrap_engine).
    let ui_scaler = if context.config.capture_type == "window" {
        match crate::arknights_settings::read_pc_ui_scaler() {
            Ok(Some(value)) => value.clamp(0.0, 1.0),
            Ok(None) => {
                log::info!("Arknights PC uiScaler registry value not found; using config value");
                context.config.effective_ui_scaler()
            }
            Err(error) => {
                log::warn!("failed to read Arknights PC uiScaler: {error}; using config value");
                context.config.effective_ui_scaler()
            }
        }
    } else {
        // `ui_scaler` is a PC-only Arknights setting; emulator/ADB/replay
        // capture always uses the reference layout (scaler = 1.0). Using a
        // stale config value here would shift the cost-bar ROI off the bar.
        ruler_core::analysis::roi::DEFAULT_UI_SCALER
    };

    // Start the Layer 1 capture pipeline.
    let capture_config = context
        .config
        .to_capture_config()
        .map_err(|e| e.to_string())?;
    let spill_dir = context.log_session_dir.join("frame_spill");
    let pipeline_config =
        PipelineConfig::new(capture_config, spill_dir, context.session_id.clone())
            .with_capture_delay_ms(context.config.screenshot_delay_ms);
    let (pipeline, info) = CapturePipeline::start(pipeline_config).map_err(|e| e.to_string())?;
    log::info!(
        "capture pipeline started: {}x{}, pipe={}",
        info.width,
        info.height,
        info.pipe_name
    );
    telemetry::send_startup_telemetry(&context.config, &info, &context.log_session_dir);
    state.update_ui(|ui, _| {
        ui.capture_dimensions = Some((info.width, info.height));
        ui.cursor_blocked = false;
    });

    // Configure cursor guard (Windows only).
    #[cfg(windows)]
    let cursor_guard = if context.config.capture_type == "window" {
        let cursor_size = match crate::arknights_settings::read_pc_cursor_size() {
            Ok(Some(value)) => value.clamp(0.0, 1.0),
            Ok(None) => 1.0,
            Err(_) => 1.0,
        };
        Some(crate::pc_cursor_guard::SelfDrawnCursorGuard::new(
            ui_scaler,
            cursor_size,
        ))
    } else {
        None
    };

    // Spawn the Layer 2 analyzer consumer (SkipToLatest).
    let analyzer_pipe = pipeline
        .connect_consumer(
            ruler_core::pipeline::cursor::ConsumerPolicy::SkipToLatest,
            0,
        )
        .map_err(|e| format!("failed to connect analyzer consumer: {e}"))?;
    let (analyzer_tx, analyzer_rx) = std::sync::mpsc::channel::<AnalyzerCommand>();
    let initial_cal_path = context
        .active_profile
        .as_ref()
        .map(|name| context.profiles.calibration_path(name));
    let analyzer_config = AnalyzerConfig {
        display_mode: context.display_mode,
        ui_scaler,
        calibration_path: initial_cal_path,
        pipeline_info: info.clone(),
        telemetry_stats: Arc::clone(&context.telemetry_stats),
        #[cfg(windows)]
        cursor_guard,
    };
    let analyzer = AnalyzerConsumer::spawn(
        analyzer_pipe,
        Arc::clone(&state),
        analyzer_config,
        analyzer_rx,
        None, // debug recorder is handled separately below
    )?;

    // Spawn the debug recorder consumer if enabled.
    if context.config.debug_recording_enabled {
        let record_video = context.config.debug_recording_video;
        let record_csv = context.config.debug_recording_csv;
        if record_video || record_csv {
            let _ = std::fs::create_dir_all(&context.log_session_dir);
            let recorder_pipe = pipeline
                .connect_consumer(ruler_core::pipeline::cursor::ConsumerPolicy::InOrder, 0)
                .map_err(|e| format!("failed to connect debug recorder consumer: {e}"))?;
            let recorder_config = DebugRecorderConfig {
                output_dir: context.log_session_dir.clone(),
                record_video,
                record_csv,
                width: info.width,
                height: info.height,
                format: ruler_core::PixelFormat::Rgba, // pipeline always captures RGBA
            };
            match DebugRecorderConsumer::spawn(recorder_pipe, recorder_config) {
                Ok(consumer) => {
                    log::info!("debug recorder consumer started");
                    context.debug_recorder = Some(consumer);
                    if record_video {
                        context.config.debug_recording_video = false;
                        context.config.debug_recording_enabled = context.config.debug_recording_csv;
                        persist_config(context, "consume one-shot MKV recording");
                    }
                }
                Err(e) => {
                    log::error!("failed to start debug recorder consumer: {e}");
                }
            }
        }
    }

    context.pipeline = Some(pipeline);
    context.pipeline_info = Some(info);
    context.analyzer_command_tx = Some(analyzer_tx);
    context.analyzer = Some(analyzer);

    if context.active_profile.is_some() {
        publish_running_state(&state, context);
    } else {
        publish_idle(&state, context);
    }

    Ok(())
}

fn drain_commands(
    state: &SharedAppState,
    context: &mut WorkerContext,
    commands: &Receiver<UiCommand>,
    running: &AtomicBool,
) -> bool {
    loop {
        match commands.try_recv() {
            Ok(command) => {
                if !handle_command(state, context, command, running) {
                    return false;
                }
            }
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => return false,
        }
    }
}

fn handle_command(
    state: &SharedAppState,
    context: &mut WorkerContext,
    command: UiCommand,
    running: &AtomicBool,
) -> bool {
    match command {
        UiCommand::PrepareCalibration => {
            log::info!("worker command: prepare calibration");
            state.clear_api_frame_history();
            context.active_profile = None;
            context.config.active_calibration_profile = None;
            send_analyzer(context, AnalyzerCommand::ClearCalibration);
            send_analyzer(
                context,
                AnalyzerCommand::SetCalibrating { calibrating: false },
            );
            persist_config(context, "prepare calibration");
            state.update_ui(|ui, api| {
                ui.mode = OverlayMode::PreCalibration;
                ui.message.clear();
                ui.progress_percent = 0.0;
                ui.active_profile = None;
                ui.total_frames_in_cycle = 0;
                ui.can_undo_reset = false;
                ui.cursor_blocked = false;
                ui.profiles = context.profiles.list(None);
                api.is_running = false;
                api.current_frame = None;
                api.active_profile = None;
                api.clear_frame_metadata();
            });
        }
        UiCommand::StartCalibration => {
            log::info!("worker command: start calibration");
            match run_calibration(state, context) {
                Ok(()) => publish_running_state(state, context),
                Err(ref error) if error == calibration::CALIBRATION_CANCELLED => {
                    log::info!("calibration cancelled by user, returning to PreCalibration");
                    let _ = state.take_cancel_calibration();
                    state.update_ui(|ui, api| {
                        ui.mode = OverlayMode::PreCalibration;
                        ui.progress_percent = 0.0;
                        ui.message.clear();
                        ui.display_frame = "--".to_string();
                        ui.display_total = "/--".to_string();
                        ui.time_str = "00:00:00".to_string();
                        ui.lap_frames = None;
                        ui.can_undo_reset = false;
                        ui.cursor_blocked = false;
                        ui.profiles = context.profiles.list(None);
                        api.is_running = false;
                        api.current_frame = None;
                        api.total_frames_in_cycle = 0;
                        api.total_elapsed_frames = 0;
                        api.clear_frame_metadata();
                    });
                }
                Err(error) => publish_error(state, context, format!("calibration failed: {error}")),
            }
        }
        UiCommand::UseProfile { filename } => {
            log::info!("worker command: use profile '{}'", filename);
            let cal_path = context.profiles.calibration_path(&filename);
            match std::fs::metadata(&cal_path) {
                Ok(_) => {
                    state.clear_api_frame_history();
                    context.active_profile = Some(filename.clone());
                    context.config.active_calibration_profile = Some(filename);
                    send_analyzer(context, AnalyzerCommand::LoadCalibration { path: cal_path });
                    persist_config(context, "select profile");
                    publish_running_state(state, context);
                }
                Err(e) => publish_error(state, context, format!("failed to load profile: {e}")),
            }
        }
        UiCommand::RenameProfile { old, new_base } => {
            log::info!("worker command: rename profile '{}' -> '{}'", old, new_base);
            match context.profiles.rename(&old, &new_base) {
                Ok(new_filename) => {
                    if context.active_profile.as_deref() == Some(old.as_str()) {
                        context.active_profile = Some(new_filename.clone());
                        context.config.active_calibration_profile = Some(new_filename);
                        persist_config(context, "rename active profile");
                    }
                    publish_current_state(state, context);
                }
                Err(error) => {
                    publish_error(state, context, format!("failed to rename profile: {error}"))
                }
            }
        }
        UiCommand::DeleteProfile { filename } => {
            log::info!("worker command: delete profile '{}'", filename);
            if let Err(error) = context.profiles.delete(&filename) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    publish_error(state, context, format!("failed to delete profile: {error}"));
                    return true;
                }
            }
            if context.active_profile.as_deref() == Some(filename.as_str()) {
                state.clear_api_frame_history();
                context.active_profile = None;
                context.config.active_calibration_profile = None;
                send_analyzer(context, AnalyzerCommand::ClearCalibration);
                send_analyzer(context, AnalyzerCommand::ResetTimer);
                persist_config(context, "delete active profile");
                publish_idle(state, context);
            } else {
                publish_current_state(state, context);
            }
        }
        UiCommand::SetDisplayMode(mode) => {
            log::info!("worker command: set display mode '{}'", mode.as_config());
            context.display_mode = mode;
            context.config.frame_display_mode = Some(mode.as_config().to_string());
            send_analyzer(context, AnalyzerCommand::SetDisplayMode(mode));
            persist_config(context, "set display mode");
            publish_current_state(state, context);
        }
        UiCommand::AdjustTimer { frames } => {
            log::info!("worker command: adjust timer by {frames} frames");
            send_analyzer(context, AnalyzerCommand::AdjustTimer { frames });
        }
        UiCommand::ResetTimer => {
            log::info!("worker command: reset timer");
            state.clear_api_frame_history();
            send_analyzer(context, AnalyzerCommand::ResetTimer);
        }
        UiCommand::UndoResetTimer => {
            log::info!("worker command: undo timer reset");
            send_analyzer(context, AnalyzerCommand::UndoResetTimer);
        }
        UiCommand::ToggleLapTimer => {
            log::info!("worker command: toggle lap timer");
            send_analyzer(context, AnalyzerCommand::ToggleLapTimer);
        }
        UiCommand::SetOverlayScale(mult) => {
            let pct = (mult * 100.0).round().clamp(50.0, 400.0) as u16;
            log::info!("worker command: set overlay scale to {}%", pct);
            context.config.overlay_scale = Some(mult);
            persist_config(context, "set overlay scale");
            state.update_ui(|ui, _| ui.overlay_scale_pct = pct);
        }
        UiCommand::SaveOverlayPlacement { x, y } => {
            log::debug!("worker command: save overlay placement x={x}, y={y}");
            context.config.overlay_pos_x = Some(x);
            context.config.overlay_pos_y = Some(y);
            persist_config(context, "save overlay placement");
        }
        UiCommand::Exit => {
            log::info!("worker command: exit");
            running.store(false, Ordering::Relaxed);
            state.request_exit();
            return false;
        }
    }

    true
}

fn send_analyzer(context: &WorkerContext, cmd: AnalyzerCommand) {
    if let Some(tx) = &context.analyzer_command_tx {
        let _ = tx.send(cmd);
    }
}

fn run_calibration(state: &SharedAppState, context: &mut WorkerContext) -> Result<(), String> {
    let _ = state.take_cancel_calibration();
    state.clear_api_frame_history();

    if context.pipeline.is_none() {
        return Err("capture pipeline is not running".to_string());
    }

    // Tell the analyzer to pause publishing during calibration.
    send_analyzer(
        context,
        AnalyzerCommand::SetCalibrating { calibrating: true },
    );
    send_analyzer(context, AnalyzerCommand::ResetTimer);

    state.update_ui(|ui, api| {
        ui.mode = OverlayMode::Calibrating;
        ui.progress_percent = 0.0;
        ui.message.clear();
        ui.display_frame = "--".to_string();
        ui.display_total = "/--".to_string();
        ui.time_str = "00:00:00".to_string();
        ui.lap_frames = None;
        ui.can_undo_reset = false;
        ui.cursor_blocked = false;
        api.is_running = false;
        api.current_frame = None;
        api.total_frames_in_cycle = 0;
        api.total_elapsed_frames = 0;
        api.clear_frame_metadata();
    });

    // Connect a fresh InOrder consumer for calibration, starting at the
    // current latest frame. Frame 0 was released long ago by the janitor, and
    // calibration only needs consecutive frames from "now" onward.
    let pipeline = context.pipeline.as_ref().ok_or("pipeline missing")?;
    let start_frame = pipeline.latest_frame_id();
    let cal_pipe = pipeline
        .connect_consumer(
            ruler_core::pipeline::cursor::ConsumerPolicy::InOrder,
            start_frame,
        )
        .map_err(|e| format!("failed to connect calibration consumer: {e}"))?;

    // Get ROI from pipeline dimensions.
    let info = context
        .pipeline_info
        .clone()
        .ok_or("pipeline info missing")?;
    let roi = roi::find_cost_bar_roi_with_ui_scaler(
        info.width as i32,
        info.height as i32,
        context.config.resolved_ui_scaler(),
    );

    let result = calibration::collect_calibration_samples(
        cal_pipe,
        state,
        roi,
        context.config.resolved_ui_scaler(),
    );

    // Re-check the cancel flag right after collection.
    if state.take_cancel_calibration() {
        send_analyzer(
            context,
            AnalyzerCommand::SetCalibrating { calibrating: false },
        );
        return Err(calibration::CALIBRATION_CANCELLED.to_string());
    }

    let (cycle_samples, screen_width, screen_height, total_bar_width) = result?;

    let calibration_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system clock error: {e}"))?
        .as_secs_f64();
    let calibration_data = calibration::infer_calibration(
        &cycle_samples,
        screen_width,
        screen_height,
        context.config.resolved_ui_scaler(),
        total_bar_width,
        calibration_time,
    )?;
    let basename = format!("profile_{}", calibration_time.trunc() as u64);
    let filename = context
        .profiles
        .save_calibration(&calibration_data, &basename)
        .map_err(|e| format!("failed to save calibration: {e}"))?;

    // Load the new profile into the analyzer and resume publishing.
    let cal_path = context.profiles.calibration_path(&filename);
    send_analyzer(context, AnalyzerCommand::LoadCalibration { path: cal_path });
    send_analyzer(context, AnalyzerCommand::ResetTimer);
    send_analyzer(
        context,
        AnalyzerCommand::SetCalibrating { calibrating: false },
    );

    context.active_profile = Some(filename.clone());
    context.config.active_calibration_profile = Some(filename);
    context
        .config
        .save_to_path(&context.config_path)
        .map_err(|e| e.to_string())?;
    log::info!(
        "saved new calibration profile selection to '{}'",
        context.config_path.display()
    );
    Ok(())
}

fn publish_current_state(state: &SharedAppState, context: &WorkerContext) {
    if context.active_profile.is_some() {
        publish_running_state(state, context);
    } else {
        publish_idle(state, context);
    }
}

fn publish_running_state(state: &SharedAppState, context: &WorkerContext) {
    let active_profile = context.active_profile.clone();
    state.update_ui(|ui, api| {
        ui.mode = OverlayMode::Running;
        ui.message.clear();
        ui.progress_percent = 0.0;
        ui.display_mode = context.display_mode;
        ui.active_profile = active_profile.clone();
        ui.profiles = context.profiles.list(active_profile.as_deref());
        ui.cursor_blocked = false;
        api.active_profile = active_profile.as_deref().map(calibration_basename);
    });
}

fn publish_idle(state: &SharedAppState, context: &WorkerContext) {
    state.update_ui(|ui, api| {
        ui.mode = OverlayMode::Idle;
        ui.message.clear();
        ui.progress_percent = 0.0;
        ui.display_mode = context.display_mode;
        ui.display_frame = "--".to_string();
        ui.display_total = "/--".to_string();
        ui.time_str = "00:00:00".to_string();
        ui.lap_frames = None;
        ui.can_undo_reset = false;
        ui.cursor_blocked = false;
        ui.total_frames_in_cycle = 0;
        ui.active_profile = None;
        ui.profiles = context.profiles.list(None);
        api.is_running = false;
        api.current_frame = None;
        api.total_frames_in_cycle = 0;
        api.total_elapsed_frames = 0;
        api.active_profile = None;
        api.clear_frame_metadata();
    });
}

fn publish_error(state: &SharedAppState, context: &WorkerContext, error: String) {
    log::error!("{error}");
    state.update_ui(|ui, api| {
        ui.mode = OverlayMode::Error;
        ui.message = error;
        ui.progress_percent = 0.0;
        ui.active_profile = context.active_profile.clone();
        ui.cursor_blocked = false;
        ui.profiles = context.profiles.list(context.active_profile.as_deref());
        api.is_running = false;
        api.current_frame = None;
        api.clear_frame_metadata();
    });
}

fn persist_config(context: &WorkerContext, reason: &str) {
    match context.config.save_to_path(&context.config_path) {
        Ok(()) => log::debug!(
            "saved config after {} to '{}'",
            reason,
            context.config_path.display()
        ),
        Err(error) => log::error!(
            "failed to save config after {} to '{}': {}",
            reason,
            context.config_path.display(),
            error
        ),
    }
}

fn wait_for_exit_commands(
    state: &SharedAppState,
    commands: &Receiver<UiCommand>,
    running: &AtomicBool,
    interval: Duration,
) {
    while running.load(Ordering::Relaxed) {
        match commands.try_recv() {
            Ok(UiCommand::Exit) | Err(TryRecvError::Disconnected) => {
                running.store(false, Ordering::Relaxed);
                state.request_exit();
                break;
            }
            Ok(_) | Err(TryRecvError::Empty) => {}
        }
        sleep_until(Instant::now() + interval, running);
    }
}

fn sleep_until(deadline: Instant, running: &AtomicBool) {
    while running.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        thread::sleep((deadline - now).min(Duration::from_millis(1)));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use super::*;

    #[test]
    fn update_notice_enters_snapshot_and_notifies_overlay() {
        let state = SharedAppState::default();
        let wake_count = Arc::new(AtomicUsize::new(0));
        state.set_overlay_waker(Some(Arc::new({
            let wake_count = Arc::clone(&wake_count);
            move || {
                wake_count.fetch_add(1, Ordering::SeqCst);
            }
        })));
        let notice = UpdateNotice {
            version: "2.3.0".to_string(),
            release_title: "Ruler 2.3.0".to_string(),
            html_url: "https://github.com/ZeroAd-06/ArknightsCostBarRuler/releases/tag/20260627"
                .to_string(),
            download_url: None,
        };

        state.set_update_notice(Some(notice.clone()));

        assert_eq!(state.snapshot().ui.update_notice, Some(notice));
        assert_eq!(wake_count.load(Ordering::SeqCst), 1);
    }
}
