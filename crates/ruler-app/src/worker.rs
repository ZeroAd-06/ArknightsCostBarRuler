use std::{
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

use ruler_core::{
    analysis::{
        calibration::infer_calibration_from_samples_with_ui_scaler_and_total_bar_width,
        scanner::{self, BattleState},
    },
    RulerConfig, RulerEngine,
};

use crate::{
    commands::UiCommand,
    debug_recorder::DebugRecorder,
    profiles::{calibration_basename, ProfileStore},
    resources::ResourceLocator,
    ui_state::{
        format_time_from_frames, ApiStateSnapshot, FrameDisplayMode, OverlayMode, UiSnapshot,
    },
};

const CALIBRATION_CYCLES: usize = 2;

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
    overlay_waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
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

struct WorkerContext {
    config: RulerConfig,
    config_path: PathBuf,
    profiles: ProfileStore,
    engine: RulerEngine,
    connected: bool,
    active_profile: Option<String>,
    display_mode: FrameDisplayMode,
    lap_start_frame: Option<i32>,
    timer_reset_undo: TimerResetUndo,
    last_elapsed_frames: i32,
    last_total_frames: i32,
    last_cost_is_negative: bool,
    sample_index: u64,
    debug_recorder: Option<DebugRecorder>,
    log_session_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct TimerResetUndo {
    previous_elapsed_frames: Option<i32>,
}

impl TimerResetUndo {
    fn remember_reset(&mut self, elapsed_frames: i32) {
        self.previous_elapsed_frames = (elapsed_frames != 0).then_some(elapsed_frames);
    }

    fn take(&mut self) -> Option<i32> {
        self.previous_elapsed_frames.take()
    }

    fn clear(&mut self) {
        self.previous_elapsed_frames = None;
    }

    fn is_available(self) -> bool {
        self.previous_elapsed_frames.is_some()
    }
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

    let mut context = WorkerContext {
        display_mode: FrameDisplayMode::from_config(config.frame_display_mode.as_deref()),
        active_profile: config.active_calibration_profile.clone(),
        config,
        config_path: resources.config_path(),
        profiles,
        engine: RulerEngine::new(),
        connected: false,
        lap_start_frame: None,
        timer_reset_undo: TimerResetUndo::default(),
        last_elapsed_frames: 0,
        last_total_frames: 0,
        last_cost_is_negative: false,
        sample_index: 0,
        debug_recorder: None,
        log_session_dir,
    };

    if let Err(error) = bootstrap_engine(&mut context, &state) {
        publish_error(&state, &context, error);
    }

    let mut next_poll_at = Instant::now();
    while running.load(Ordering::Relaxed) {
        if !drain_commands(&state, &mut context, &commands, &running) {
            break;
        }

        if context.connected && context.active_profile.is_some() {
            analyze_once(&state, &mut context);
        }

        next_poll_at = next_poll_at.max(Instant::now()) + interval;
        sleep_until(next_poll_at, &running);
    }
}

fn bootstrap_engine(context: &mut WorkerContext, state: &SharedAppState) -> Result<(), String> {
    context
        .engine
        .set_ui_scaler(context.config.effective_ui_scaler());
    let capture_config = context
        .config
        .to_capture_config()
        .map_err(|error| error.to_string())?;
    log::info!(
        "connecting capture backend for '{}'",
        context.config.capture_type
    );
    let dims = context.engine.connect(capture_config)?;
    context.connected = true;
    log::info!("capture backend connected: {}x{}", dims.0, dims.1);
    state.update_ui(|ui, _| {
        ui.capture_dimensions = Some(dims);
    });

    if let Some(profile) = context.active_profile.clone() {
        load_profile(context, &profile)?;
        publish_running_state(state, context, None);
    } else {
        publish_idle(state, context);
    }

    Ok(())
}

fn load_profile(context: &mut WorkerContext, filename: &str) -> Result<(), String> {
    let calibration_path = context.profiles.calibration_path(filename);
    log::info!(
        "loading calibration profile '{}' from '{}'",
        filename,
        calibration_path.display()
    );
    context
        .engine
        .load_calibration(&calibration_path)
        .map_err(|error| {
            format!(
                "failed to load calibration '{}': {error}",
                calibration_path.display()
            )
        })?;
    context.active_profile = Some(filename.to_string());
    context.lap_start_frame = None;
    context.timer_reset_undo.clear();
    context.last_elapsed_frames = 0;
    context.last_cost_is_negative = false;
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
            context.active_profile = None;
            context.config.active_calibration_profile = None;
            context.lap_start_frame = None;
            context.timer_reset_undo.clear();
            context.last_cost_is_negative = false;
            persist_config(context, "prepare calibration");
            state.update_ui(|ui, api| {
                ui.mode = OverlayMode::PreCalibration;
                ui.message.clear();
                ui.progress_percent = 0.0;
                ui.active_profile = None;
                ui.total_frames_in_cycle = 0;
                ui.can_undo_reset = false;
                ui.profiles = context.profiles.list(None);
                api.is_running = false;
                api.current_frame = None;
                api.active_profile = None;
            });
        }
        UiCommand::StartCalibration => {
            log::info!("worker command: start calibration");
            match run_calibration(state, context) {
                Ok(()) => publish_running_state(state, context, None),
                Err(error) => publish_error(state, context, format!("calibration failed: {error}")),
            }
        }
        UiCommand::UseProfile { filename } => {
            log::info!("worker command: use profile '{}'", filename);
            match load_profile(context, &filename) {
                Ok(()) => {
                    context.config.active_calibration_profile = Some(filename);
                    persist_config(context, "select profile");
                    publish_running_state(state, context, None);
                }
                Err(error) => publish_error(state, context, error),
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
                context.active_profile = None;
                context.config.active_calibration_profile = None;
                context.engine.reset_timer();
                context.last_elapsed_frames = 0;
                context.lap_start_frame = None;
                context.timer_reset_undo.clear();
                context.last_cost_is_negative = false;
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
            persist_config(context, "set display mode");
            publish_current_state(state, context);
        }
        UiCommand::AdjustTimer { frames } => {
            log::info!("worker command: adjust timer by {frames} frames");
            context.engine.adjust_timer(frames);
            context.last_elapsed_frames += frames;
            publish_current_state(state, context);
        }
        UiCommand::ResetTimer => {
            log::info!("worker command: reset timer");
            context
                .timer_reset_undo
                .remember_reset(context.last_elapsed_frames);
            context.engine.reset_timer();
            context.lap_start_frame = None;
            context.last_elapsed_frames = 0;
            publish_current_state(state, context);
        }
        UiCommand::UndoResetTimer => {
            log::info!("worker command: undo timer reset");
            if let Some(elapsed_frames) = context.timer_reset_undo.take() {
                set_timer_elapsed(context, elapsed_frames);
                context.lap_start_frame = None;
                publish_current_state(state, context);
            }
        }
        UiCommand::ToggleLapTimer => {
            log::info!("worker command: toggle lap timer");
            context.lap_start_frame = if context.lap_start_frame.is_some() {
                None
            } else {
                Some(context.last_elapsed_frames)
            };
            publish_current_state(state, context);
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

fn run_calibration(state: &SharedAppState, context: &mut WorkerContext) -> Result<(), String> {
    if !context.connected {
        return Err("capture backend is not connected".to_string());
    }

    context.engine.reset_timer();
    context.lap_start_frame = None;
    context.timer_reset_undo.clear();
    context.last_elapsed_frames = 0;
    context.last_total_frames = 0;
    context.last_cost_is_negative = false;
    state.update_ui(|ui, api| {
        ui.mode = OverlayMode::Calibrating;
        ui.progress_percent = 0.0;
        ui.message.clear();
        ui.display_frame = "--".to_string();
        ui.display_total = "/--".to_string();
        ui.time_str = "00:00:00".to_string();
        ui.lap_frames = None;
        ui.can_undo_reset = false;
        api.is_running = false;
        api.current_frame = None;
        api.total_frames_in_cycle = 0;
        api.total_elapsed_frames = 0;
    });

    let (cycle_samples, screen_width, screen_height, total_bar_width) =
        collect_calibration_samples(state, context)?;
    let calibration_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock error: {error}"))?
        .as_secs_f64();
    let calibration_data = infer_calibration_from_samples_with_ui_scaler_and_total_bar_width(
        &cycle_samples,
        screen_width,
        screen_height,
        context.config.effective_ui_scaler(),
        total_bar_width,
        calibration_time,
    )?;
    let basename = format!("profile_{}", calibration_time.trunc() as u64);
    let filename = context
        .profiles
        .save_calibration(&calibration_data, &basename)
        .map_err(|error| format!("failed to save calibration: {error}"))?;

    context.engine.reset_timer();
    load_profile(context, &filename)?;
    context.config.active_calibration_profile = Some(filename);
    context
        .config
        .save_to_path(&context.config_path)
        .map_err(|error| error.to_string())?;
    log::info!(
        "saved new calibration profile selection to '{}'",
        context.config_path.display()
    );
    Ok(())
}

fn collect_calibration_samples(
    state: &SharedAppState,
    context: &mut WorkerContext,
) -> Result<(Vec<Vec<i32>>, u32, u32, i32), String> {
    let first_frame = context.engine.capture_frame()?;
    let screen_width = first_frame.width;
    let screen_height = first_frame.height;
    let roi = context
        .engine
        .roi()
        .ok_or_else(|| "capture ROI is not ready".to_string())?;
    let total_bar_width = roi.1 - roi.0;
    if total_bar_width <= 0 {
        return Err("capture ROI has invalid width".to_string());
    }

    let mut cycle_samples = Vec::new();
    let mut current_cycle_data = Vec::new();
    let mut previous_cost_state_raw = None;
    let mut is_collecting_cycle = false;
    let mut progress = CalibrationProgress::new(total_bar_width);
    let mut frame = first_frame;

    while cycle_samples.len() < CALIBRATION_CYCLES {
        let current_cost_state_raw = scanner::get_raw_filled_pixel_width(
            &frame.data,
            frame.width,
            frame.height,
            frame.format,
            roi,
        );

        if let Some(current) = current_cost_state_raw {
            if let Some(previous) = previous_cost_state_raw {
                if (previous as f64) > total_bar_width as f64 * 0.9
                    && (current as f64) < total_bar_width as f64 * 0.1
                {
                    is_collecting_cycle = true;
                    if !current_cycle_data.is_empty() {
                        cycle_samples.push(std::mem::take(&mut current_cycle_data));
                    }
                }
            }

            if is_collecting_cycle && cycle_samples.len() < CALIBRATION_CYCLES {
                current_cycle_data.push(current);
            }

            let progress_percent =
                progress.update(current, cycle_samples.len(), is_collecting_cycle);
            state.update_ui(|ui, _| {
                ui.mode = OverlayMode::Calibrating;
                ui.progress_percent = progress_percent;
                ui.message.clear();
            });
            previous_cost_state_raw = Some(current);
        } else {
            previous_cost_state_raw = None;
        }

        if cycle_samples.len() < CALIBRATION_CYCLES {
            frame = context.engine.capture_frame()?;
        }
    }

    state.update_ui(|ui, _| {
        ui.mode = OverlayMode::Calibrating;
        ui.progress_percent = 100.0;
    });
    Ok((cycle_samples, screen_width, screen_height, total_bar_width))
}

#[derive(Debug)]
struct CalibrationProgress {
    total_bar_width: i32,
    initial_width: Option<i32>,
    last_percent: f32,
}

impl CalibrationProgress {
    fn new(total_bar_width: i32) -> Self {
        Self {
            total_bar_width: total_bar_width.max(1),
            initial_width: None,
            last_percent: 0.0,
        }
    }

    fn update(&mut self, current_width: i32, completed_cycles: usize, collecting: bool) -> f32 {
        let current = current_width.clamp(0, self.total_bar_width);
        let initial = *self.initial_width.get_or_insert(current);
        let wait_units = (self.total_bar_width - initial).max(0) as f32;
        let total_bar_width = self.total_bar_width as f32;
        let total_units = wait_units + CALIBRATION_CYCLES as f32 * total_bar_width;

        let completed_units = if collecting {
            let completed_cycles = completed_cycles.min(CALIBRATION_CYCLES);
            let current_cycle_units = if completed_cycles < CALIBRATION_CYCLES {
                current as f32
            } else {
                0.0
            };
            wait_units + completed_cycles as f32 * total_bar_width + current_cycle_units
        } else {
            (current - initial).clamp(0, self.total_bar_width) as f32
        };

        let percent = if total_units > 0.0 {
            completed_units / total_units * 100.0
        } else {
            0.0
        }
        .clamp(0.0, 100.0);

        self.last_percent = self.last_percent.max(percent);
        self.last_percent
    }
}

fn analyze_once(state: &SharedAppState, context: &mut WorkerContext) {
    context.sample_index += 1;

    // Time the capture for debug recording
    let capture_start = Instant::now();

    match context.engine.capture_frame() {
        Ok(frame_data) => {
            let capture_dur_us = capture_start.elapsed().as_micros();

            // Debug recording: lazy-init on first frame
            if context.debug_recorder.is_none()
                && context.config.debug_recording_enabled
                && context.connected
            {
                let record_video = context.config.debug_recording_video;
                let record_csv = context.config.debug_recording_csv;
                if record_video || record_csv {
                    // Ensure output directory exists
                    let _ = std::fs::create_dir_all(&context.log_session_dir);
                    match DebugRecorder::start(
                        &context.log_session_dir,
                        record_video,
                        record_csv,
                        frame_data.width,
                        frame_data.height,
                        frame_data.format,
                    ) {
                        Ok(recorder) => {
                            log::info!(
                                "debug recording started in '{}'",
                                context.log_session_dir.display()
                            );
                            context.debug_recorder = Some(recorder);
                            if record_video {
                                context.config.debug_recording_video = false;
                                context.config.debug_recording_enabled =
                                    context.config.debug_recording_csv;
                                persist_config(context, "consume one-shot MKV recording");
                            }
                        }
                        Err(e) => {
                            log::error!("debug recording failed to start: {e}");
                        }
                    }
                }
            }

            if let Some(ref mut recorder) = context.debug_recorder {
                recorder.record_video_frame(&frame_data);
            }

            match context.engine.analyze_captured_frame(&frame_data) {
                Ok(result) => {
                    // Debug recording: write analysis row after the video frame has already been queued.
                    if let Some(ref mut recorder) = context.debug_recorder {
                        recorder.record_analysis_row(&result, capture_dur_us);
                    }

                    let worker_timing = WorkerTimingSnapshot {
                        sample_index: context.sample_index,
                    };
                    log::trace!(
                        "worker frame {} => battle_state={}, logical_frame={:?}, total={}, raw_width={:?}, elapsed={}, negative={}",
                        context.sample_index,
                        result.battle_state.as_str(),
                        result.logical_frame,
                        result.total_frames_in_cycle,
                        result.raw_pixel_width,
                        result.elapsed_frames,
                        result.cost_is_negative
                    );
                    context.last_total_frames = result.total_frames_in_cycle;
                    context.last_cost_is_negative = result.cost_is_negative;
                    if result.battle_state == BattleState::BattleBegin {
                        if context.last_elapsed_frames != result.elapsed_frames {
                            context
                                .timer_reset_undo
                                .remember_reset(context.last_elapsed_frames);
                        }
                        context.last_elapsed_frames = result.elapsed_frames;
                        context.lap_start_frame = None;
                    } else if result.logical_frame.is_some() {
                        context.last_elapsed_frames = result.elapsed_frames;
                    }
                    let display_frame = context.display_mode.display_frame(result.logical_frame);
                    let display_total = if result.total_frames_in_cycle > 0 {
                        display_total_with_cost_marker(
                            context.display_mode,
                            result.total_frames_in_cycle,
                            result.cost_is_negative,
                        )
                    } else {
                        "/--".to_string()
                    };
                    let lap_frames = context
                        .lap_start_frame
                        .map(|start| context.last_elapsed_frames - start);
                    let active_profile = context.active_profile.clone();
                    let active_basename = active_profile.as_deref().map(calibration_basename);
                    state.update_ui(|ui, api| {
                        ui.mode = OverlayMode::Running;
                        ui.message.clear();
                        ui.display_mode = context.display_mode;
                        ui.display_frame = display_frame;
                        ui.display_total = display_total;
                        ui.time_str = format_time_from_frames(context.last_elapsed_frames);
                        ui.lap_frames = lap_frames;
                        ui.can_undo_reset = timer_reset_undo_enabled(context);
                        ui.total_frames_in_cycle = result.total_frames_in_cycle;
                        ui.active_profile = active_profile.clone();
                        ui.profiles = context.profiles.list(active_profile.as_deref());
                        api.is_running = result.logical_frame.is_some();
                        api.current_frame = result.logical_frame;
                        api.total_frames_in_cycle = if result.logical_frame.is_some() {
                            result.total_frames_in_cycle
                        } else {
                            0
                        };
                        api.total_elapsed_frames = context.last_elapsed_frames;
                        api.active_profile = active_basename.clone();
                    });
                    state.update_timing(worker_timing);
                }
                Err(error) => publish_error(state, context, format!("analyze error: {error}")),
            }
        }
        Err(error) => publish_error(state, context, format!("capture error: {error}")),
    }
}

fn publish_current_state(state: &SharedAppState, context: &WorkerContext) {
    if context.active_profile.is_some() {
        publish_running_state(state, context, None);
    } else {
        publish_idle(state, context);
    }
}

fn publish_running_state(state: &SharedAppState, context: &WorkerContext, frame: Option<i32>) {
    let active_profile = context.active_profile.clone();
    let total_frames = context.last_total_frames.max(0);
    let lap_frames = context
        .lap_start_frame
        .map(|start| context.last_elapsed_frames - start);
    state.update_ui(|ui, api| {
        ui.mode = OverlayMode::Running;
        ui.message.clear();
        ui.progress_percent = 0.0;
        ui.display_mode = context.display_mode;
        ui.display_frame = context.display_mode.display_frame(frame);
        ui.display_total = if total_frames > 0 {
            display_total_with_cost_marker(
                context.display_mode,
                total_frames,
                context.last_cost_is_negative,
            )
        } else {
            "/--".to_string()
        };
        ui.time_str = format_time_from_frames(context.last_elapsed_frames);
        ui.lap_frames = lap_frames;
        ui.can_undo_reset = timer_reset_undo_enabled(context);
        ui.total_frames_in_cycle = total_frames;
        ui.active_profile = active_profile.clone();
        ui.profiles = context.profiles.list(active_profile.as_deref());
        api.is_running = frame.is_some();
        api.current_frame = frame;
        api.total_frames_in_cycle = if frame.is_some() { total_frames } else { 0 };
        api.total_elapsed_frames = context.last_elapsed_frames;
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
        ui.total_frames_in_cycle = 0;
        ui.active_profile = None;
        ui.profiles = context.profiles.list(None);
        api.is_running = false;
        api.current_frame = None;
        api.total_frames_in_cycle = 0;
        api.total_elapsed_frames = 0;
        api.active_profile = None;
    });
}

fn publish_error(state: &SharedAppState, context: &WorkerContext, error: String) {
    log::error!("{error}");
    state.update_ui(|ui, api| {
        ui.mode = OverlayMode::Error;
        ui.message = error;
        ui.progress_percent = 0.0;
        ui.active_profile = context.active_profile.clone();
        ui.can_undo_reset = timer_reset_undo_enabled(context);
        ui.profiles = context.profiles.list(context.active_profile.as_deref());
        api.is_running = false;
        api.current_frame = None;
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

fn set_timer_elapsed(context: &mut WorkerContext, elapsed_frames: i32) {
    context
        .engine
        .adjust_timer(elapsed_frames - context.last_elapsed_frames);
    context.last_elapsed_frames = elapsed_frames;
}

fn timer_reset_undo_enabled(context: &WorkerContext) -> bool {
    context.active_profile.is_some() && context.timer_reset_undo.is_available()
}

fn display_total_with_cost_marker(
    display_mode: FrameDisplayMode,
    total_frames: i32,
    cost_is_negative: bool,
) -> String {
    let mut display_total = display_mode.display_total(total_frames);
    if cost_is_negative {
        display_total.push('*');
    }
    display_total
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
    use super::{CalibrationProgress, TimerResetUndo};

    fn assert_near(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.001,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn timer_reset_undo_remembers_nonzero_elapsed_once() {
        let mut undo = TimerResetUndo::default();

        undo.remember_reset(42);

        assert!(undo.is_available());
        assert_eq!(undo.take(), Some(42));
        assert!(!undo.is_available());
        assert_eq!(undo.take(), None);
    }

    #[test]
    fn timer_reset_undo_ignores_zero_elapsed() {
        let mut undo = TimerResetUndo::default();

        undo.remember_reset(0);

        assert!(!undo.is_available());
        assert_eq!(undo.take(), None);
    }

    #[test]
    fn calibration_progress_counts_initial_remaining_bar_before_two_cycles() {
        let mut progress = CalibrationProgress::new(100);

        assert_near(progress.update(50, 0, false), 0.0);
        assert_near(progress.update(75, 0, false), 10.0);
        assert_near(progress.update(0, 0, true), 20.0);
        assert_near(progress.update(50, 0, true), 40.0);
        assert_near(progress.update(0, 1, true), 60.0);
        assert_near(progress.update(50, 1, true), 80.0);
        assert_near(progress.update(0, 2, true), 100.0);
    }

    #[test]
    fn calibration_progress_starts_at_zero_when_already_empty() {
        let mut progress = CalibrationProgress::new(100);

        assert_near(progress.update(0, 0, false), 0.0);
        assert_near(progress.update(50, 0, false), 16.666_668);
        assert_near(progress.update(0, 0, true), 33.333_336);
        assert_near(progress.update(0, 1, true), 66.666_67);
        assert_near(progress.update(0, 2, true), 100.0);
    }

    #[test]
    fn calibration_progress_does_not_go_backwards_on_width_jitter() {
        let mut progress = CalibrationProgress::new(100);

        assert_near(progress.update(50, 0, false), 0.0);
        assert_near(progress.update(80, 0, false), 12.0);
        assert_near(progress.update(70, 0, false), 12.0);
        assert_near(progress.update(0, 0, true), 20.0);
    }
}
