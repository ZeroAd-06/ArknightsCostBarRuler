use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use ruler_core::{CaptureConfig, RulerConfig, RulerEngine};

#[derive(Clone, Debug, Default)]
pub struct ApiStateSnapshot {
    pub is_running: bool,
    pub current_frame: Option<i32>,
    pub total_frames_in_cycle: i32,
    pub total_elapsed_frames: i32,
    pub active_profile: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WorkerTimingSnapshot {
    pub sample_index: u64,
    pub capture_started_at: Option<Instant>,
    pub capture_completed_at: Option<Instant>,
    pub analysis_completed_at: Option<Instant>,
    pub state_published_at: Option<Instant>,
    pub capture_duration: Option<Duration>,
    pub analyze_duration: Option<Duration>,
    pub total_worker_duration: Option<Duration>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OverlayTimingSnapshot {
    pub last_painted_sample_index: Option<u64>,
    pub last_painted_at: Option<Instant>,
}

#[derive(Clone, Debug)]
pub struct AppStateSnapshot {
    pub status_text: String,
    pub frame_text: String,
    pub timer_text: String,
    pub latency_text: String,
    pub api: ApiStateSnapshot,
    pub worker_timing: WorkerTimingSnapshot,
    pub overlay_timing: OverlayTimingSnapshot,
}

impl Default for AppStateSnapshot {
    fn default() -> Self {
        Self {
            status_text: "status: booting".to_string(),
            frame_text: "frame: --".to_string(),
            timer_text: "timer: --:--.--".to_string(),
            latency_text: "latency: awaiting runtime samples".to_string(),
            api: ApiStateSnapshot::default(),
            worker_timing: WorkerTimingSnapshot::default(),
            overlay_timing: OverlayTimingSnapshot::default(),
        }
    }
}

#[derive(Default)]
pub struct SharedAppState {
    inner: Mutex<AppStateSnapshot>,
    overlay_waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl std::fmt::Debug for SharedAppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedAppState")
            .field("snapshot", &self.snapshot())
            .finish_non_exhaustive()
    }
}

impl SharedAppState {
    #[must_use]
    pub fn snapshot(&self) -> AppStateSnapshot {
        self.inner.lock().expect("shared app state poisoned").clone()
    }

    pub fn update_text(&self, status_text: String, frame_text: String, timer_text: String) {
        {
            let mut state = self.inner.lock().expect("shared app state poisoned");
            state.status_text = status_text;
            state.frame_text = frame_text;
            state.timer_text = timer_text;
        }

        self.notify_overlay();
    }

    pub fn update_startup_status(&self, startup: &StartupStatus) {
        {
            let mut state = self.inner.lock().expect("shared app state poisoned");
            state.status_text = format!("status: {}", startup.phase);
            state.frame_text = startup.frame_text();
            state.timer_text = startup.timer_text();
            state.api = startup.api_snapshot();
        }

        self.notify_overlay();
    }

    pub fn update_runtime_state(
        &self,
        status_text: String,
        frame_text: String,
        timer_text: String,
        latency_text: String,
        api: ApiStateSnapshot,
        worker_timing: WorkerTimingSnapshot,
    ) {
        {
            let mut state = self.inner.lock().expect("shared app state poisoned");
            state.status_text = status_text;
            state.frame_text = frame_text;
            state.timer_text = timer_text;
            state.latency_text = latency_text;
            state.api = api;
            state.worker_timing = worker_timing;
        }

        self.notify_overlay();
    }

    pub fn record_overlay_paint(&self, sample_index: u64, painted_at: Instant) {
        let mut state = self.inner.lock().expect("shared app state poisoned");
        state.overlay_timing.last_painted_sample_index = Some(sample_index);
        state.overlay_timing.last_painted_at = Some(painted_at);

        if state.worker_timing.sample_index == sample_index {
            state.latency_text = format_latency_line(state.worker_timing, state.overlay_timing, painted_at);
        }
    }

    pub fn set_overlay_waker(&self, waker: Option<Arc<dyn Fn() + Send + Sync>>) {
        let mut overlay_waker = self.overlay_waker.lock().expect("shared app state poisoned");
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
            detail: "config.json was not found; app is idle until the config file exists.".to_string(),
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
    pub fn engine_not_connected(config_path: String, loaded_config: RulerConfig, detail: String) -> Self {
        Self {
            phase: "engine not connected".to_string(),
            detail,
            config_path,
            loaded_config: Some(loaded_config),
        }
    }

    #[must_use]
    pub fn frame_text(&self) -> String {
        if let Some(config) = &self.loaded_config {
            summarize_config(config)
        } else {
            format!("config: {}", self.config_path)
        }
    }

    #[must_use]
    pub fn timer_text(&self) -> String {
        format!("startup: {}", self.detail)
    }

    #[must_use]
    pub fn api_snapshot(&self) -> ApiStateSnapshot {
        ApiStateSnapshot {
            is_running: false,
            current_frame: None,
            total_frames_in_cycle: 0,
            total_elapsed_frames: 0,
            active_profile: self.loaded_config.as_ref().and_then(|config| config.active_calibration_profile.clone()),
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
        interval: Duration,
    ) -> Result<Self, crate::app::StartupError> {
        let running = Arc::new(AtomicBool::new(true));
        let worker_running = Arc::clone(&running);
        let handle = thread::Builder::new()
            .name("ruler-app-worker".to_string())
            .spawn(move || {
                run_worker_loop(state, startup, worker_running, interval);
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
        "background worker is active and will attempt config-driven engine bootstrap"
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

fn summarize_config(config: &RulerConfig) -> String {
    match config.capture_type.as_str() {
        "mumu" | "ldplayer" => format!(
            "config: {} instance={} path={}",
            config.capture_type,
            config.instance_index.unwrap_or(0),
            config.install_path.as_deref().unwrap_or("--")
        ),
        "window" => format!(
            "config: window title={} handle={}",
            config.window_title.as_deref().unwrap_or("--"),
            config
                .window_handle
                .map(|value| value.to_string())
                .unwrap_or_else(|| "--".to_string())
        ),
        other => format!("config: {other}"),
    }
}

fn run_worker_loop(
    state: Arc<SharedAppState>,
    startup: StartupStatus,
    running: Arc<AtomicBool>,
    interval: Duration,
) {
    state.update_startup_status(&startup);

    let Some(config) = startup.loaded_config.clone() else {
        wait_until_stopped(&running, interval);
        return;
    };

    let capture_config = match config.to_capture_config() {
        Ok(config) => config,
        Err(error) => {
            state.update_text(
                "status: config invalid".to_string(),
                format!("config: {}", startup.config_path),
                format!("startup: {error}"),
            );
            wait_until_stopped(&running, interval);
            return;
        }
    };

    let mut engine = RulerEngine::new();
    let startup_status = match bootstrap_engine(&mut engine, &config, capture_config) {
        Ok(status) => status,
        Err(error) => {
            state.update_text(
                "status: engine bootstrap failed".to_string(),
                summarize_config(&config),
                format!("startup: {error}"),
            );
            wait_until_stopped(&running, interval);
            return;
        }
    };

    state.update_runtime_state(
        startup_status.0.clone(),
        startup_status.1.clone(),
        startup_status.2.clone(),
        "latency: runtime connected; waiting for analyzed sample".to_string(),
        ApiStateSnapshot {
            is_running: false,
            current_frame: None,
            total_frames_in_cycle: 0,
            total_elapsed_frames: 0,
            active_profile: config.active_calibration_profile.clone(),
        },
        WorkerTimingSnapshot::default(),
    );

    let mut sample_index = 0_u64;
    let mut next_poll_at = Instant::now();

    while running.load(Ordering::Relaxed) {
        let poll_started_at = Instant::now();

        if startup_status.3 {
            sample_index += 1;
            let capture_started_at = poll_started_at;
            match engine.capture_frame() {
                Ok(frame_data) => {
                    let capture_completed_at = Instant::now();
                    match engine.analyze_captured_frame(&frame_data) {
                        Ok(result) => {
                            let analysis_completed_at = Instant::now();
                            let published_at = analysis_completed_at;
                            let worker_timing = WorkerTimingSnapshot {
                                sample_index,
                                capture_started_at: Some(capture_started_at),
                                capture_completed_at: Some(capture_completed_at),
                                analysis_completed_at: Some(analysis_completed_at),
                                state_published_at: Some(published_at),
                                capture_duration: Some(
                                    capture_completed_at.saturating_duration_since(capture_started_at),
                                ),
                                analyze_duration: Some(
                                    analysis_completed_at.saturating_duration_since(capture_completed_at),
                                ),
                                total_worker_duration: Some(
                                    published_at.saturating_duration_since(capture_started_at),
                                ),
                            };
                            let status = analyzed_status(&config, result, worker_timing);
                            state.update_runtime_state(
                                status.0.clone(),
                                status.1.clone(),
                                status.2.clone(),
                                status.3.clone(),
                                status.4.clone(),
                                status.5,
                            );
                            log::debug!(
                                "worker sample {} published: {}",
                                status.5.sample_index,
                                status.3
                            );
                        }
                        Err(error) => {
                            state.update_text(
                                "status: analyze error".to_string(),
                                summarize_config(&config),
                                format!("startup: {error}"),
                            );
                            break;
                        }
                    }
                }
                Err(error) => {
                    state.update_text(
                        "status: capture error".to_string(),
                        summarize_config(&config),
                        format!("startup: {error}"),
                    );
                    break;
                }
            }
        }

        next_poll_at = next_poll_at.max(poll_started_at) + interval;
        sleep_until(next_poll_at, &running);
    }
}

fn wait_until_stopped(running: &AtomicBool, interval: Duration) {
    while running.load(Ordering::Relaxed) {
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

fn bootstrap_engine(
    engine: &mut RulerEngine,
    config: &RulerConfig,
    capture_config: CaptureConfig,
) -> Result<(String, String, String, bool), String> {
    let dims = engine.connect(capture_config)?;

    if let Some(profile) = &config.active_calibration_profile {
        let calibration_path = calibration_path(profile);
        engine
            .load_calibration(&calibration_path)
            .map_err(|error| format!("failed to load calibration '{}': {error}", calibration_path.display()))?;

        Ok((
            format!("status: connected {}x{}", dims.0, dims.1),
            summarize_config(config),
            format!("startup: calibration loaded from {}", calibration_path.display()),
            true,
        ))
    } else {
        Ok((
            format!("status: connected {}x{}", dims.0, dims.1),
            summarize_config(config),
            "startup: capture connected, but no active calibration profile is configured".to_string(),
            false,
        ))
    }
}

fn analyzed_status(
    config: &RulerConfig,
    result: ruler_core::FrameResult,
    worker_timing: WorkerTimingSnapshot,
) -> (String, String, String, String, ApiStateSnapshot, WorkerTimingSnapshot) {
    let frame_display = match result.logical_frame {
        Some(frame) => format!(
            "frame: {} / {} (raw={})",
            frame,
            result.total_frames_in_cycle,
            result.raw_pixel_width
                .map(|value| value.to_string())
                .unwrap_or_else(|| "--".to_string())
        ),
        None => format!(
            "frame: -- / {} (raw={})",
            result.total_frames_in_cycle,
            result.raw_pixel_width
                .map(|value| value.to_string())
                .unwrap_or_else(|| "--".to_string())
        ),
    };

    (
        "status: running".to_string(),
        frame_display,
        format!("timer: {} frames", result.elapsed_frames),
        format_worker_latency_line(worker_timing),
        ApiStateSnapshot {
            is_running: result.logical_frame.is_some(),
            current_frame: result.logical_frame,
            total_frames_in_cycle: if result.logical_frame.is_some() {
                result.total_frames_in_cycle
            } else {
                0
            },
            total_elapsed_frames: result.elapsed_frames,
            active_profile: config.active_calibration_profile.clone(),
        },
        worker_timing,
    )
}

fn format_worker_latency_line(worker_timing: WorkerTimingSnapshot) -> String {
    let capture_text = format_duration_ms(worker_timing.capture_duration);
    let analyze_text = format_duration_ms(worker_timing.analyze_duration);
    let total_text = format_duration_ms(worker_timing.total_worker_duration);

    format!(
        "latency: worker sample={} capture={} analyze={} capture->publish={}",
        worker_timing.sample_index, capture_text, analyze_text, total_text
    )
}

fn format_latency_line(
    worker_timing: WorkerTimingSnapshot,
    overlay_timing: OverlayTimingSnapshot,
    painted_at: Instant,
) -> String {
    let capture_text = format_duration_ms(worker_timing.capture_duration);
    let analyze_text = format_duration_ms(worker_timing.analyze_duration);
    let worker_total_text = format_duration_ms(worker_timing.total_worker_duration);
    let publish_to_paint = worker_timing
        .state_published_at
        .map(|published_at| painted_at.saturating_duration_since(published_at));
    let capture_to_paint = worker_timing
        .capture_started_at
        .map(|capture_started_at| painted_at.saturating_duration_since(capture_started_at));
    let capture_complete_to_paint = worker_timing
        .capture_completed_at
        .map(|capture_completed_at| painted_at.saturating_duration_since(capture_completed_at));
    let analysis_to_paint = worker_timing
        .analysis_completed_at
        .map(|analysis_completed_at| painted_at.saturating_duration_since(analysis_completed_at));

    format!(
        "latency: sample={} capture={} analyze={} capture->publish={} capture->paint={} capture_done->paint={} publish->paint={} analyze->paint={} painted_sample={}",
        worker_timing.sample_index,
        capture_text,
        analyze_text,
        worker_total_text,
        format_duration_ms(capture_to_paint),
        format_duration_ms(capture_complete_to_paint),
        format_duration_ms(publish_to_paint),
        format_duration_ms(analysis_to_paint),
        overlay_timing
            .last_painted_sample_index
            .map(|value| value.to_string())
            .unwrap_or_else(|| "--".to_string())
    )
}

fn format_duration_ms(duration: Option<Duration>) -> String {
    duration
        .map(|value| format!("{:.1}ms", value.as_secs_f64() * 1000.0))
        .unwrap_or_else(|| "n/a".to_string())
}

fn calibration_path(profile: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("calibration")
        .join(profile)
}
