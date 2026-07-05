//! Layer 2 analyzer consumer — connects to the capture pipeline as a
//! `SkipToLatest` consumer, runs the [`Analyzer`] on each received frame,
//! and publishes [`FrameResult`]s to the shared UI state.
//!
//! This is the real-time analysis path: it always processes the most recent
//! frame available, dropping intermediate frames if the analyzer falls behind
//! the capture rate. This keeps the displayed timer and battle state
//! responsive even under heavy load.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::Receiver,
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use ruler_core::{
    analysis::scanner::{self, BattleState},
    pipeline::{frame::Frame as PipelineFrame, ConsumerPipe, PipelineInfo},
    Analyzer, PixelFormat,
};

use crate::{
    debug_recorder::DebugRecorder,
    telemetry::RunTelemetryStats,
    ui_state::{
        format_time_from_frames, ApiFrameRecord, ApiStateSnapshot, FrameDisplayMode, OverlayMode,
        ResetKind,
    },
    worker::{SharedAppState, WorkerTimingSnapshot},
};

/// Commands sent from the worker thread to the L2 analyzer consumer.
pub enum AnalyzerCommand {
    /// Load a calibration profile from the given path.
    LoadCalibration { path: std::path::PathBuf },
    /// Clear the current calibration (e.g. when entering pre-calibration mode).
    ClearCalibration,
    /// Reset the timer to zero.
    ResetTimer,
    /// Undo the last timer reset (restore the previous elapsed frame count).
    UndoResetTimer,
    /// Adjust the timer by a delta (positive or negative).
    AdjustTimer { frames: i32 },
    /// Set the timer to an absolute elapsed frame count.
    SetTimer { frames: i32 },
    /// Set the active profile index within the calibration table.
    ///
    /// Part of the consumer command protocol but not currently wired to a UI
    /// gesture — the active profile is selected automatically during
    /// calibration inference. Kept here so future callers (or external API
    /// consumers) can drive the analyzer without a protocol churn.
    #[allow(dead_code)]
    SetProfileIndex { index: usize },
    /// Set the display mode (raw frame number vs. logical frame).
    SetDisplayMode(FrameDisplayMode),
    /// Update the ROI (e.g. after ui_scaler change).
    ///
    /// Part of the consumer command protocol but not currently wired up — ROI
    /// is established at spawn from `pipeline_info` and via calibration. Kept
    /// so live ROI changes can be added without a protocol break.
    #[allow(dead_code)]
    SetRoi { width: i32, height: i32 },
    /// Toggle the lap timer on/off.
    ToggleLapTimer,
    /// Enter/exit "calibrating" mode — when calibrating, the analyzer skips
    /// publishing results so the calibration collector owns the UI.
    SetCalibrating { calibrating: bool },
    /// Stop the analyzer thread.
    Shutdown,
}

/// Configuration for spawning an [`AnalyzerConsumer`].
pub struct AnalyzerConfig {
    pub display_mode: FrameDisplayMode,
    pub ui_scaler: f64,
    /// Initial calibration path (optional — analyzer starts without
    /// calibration if `None`).
    pub calibration_path: Option<std::path::PathBuf>,
    /// Pipeline info (for window_info, used by the cursor guard).
    pub pipeline_info: PipelineInfo,
    pub telemetry_stats: Arc<RunTelemetryStats>,
    /// Windows-only cursor guard. Detects when the in-game self-drawn
    /// cursor overlaps the cost bar so the frame is skipped.
    #[cfg(windows)]
    pub cursor_guard: Option<crate::pc_cursor_guard::SelfDrawnCursorGuard>,
}

/// The L2 analyzer consumer. Runs on its own thread; communicates with the
/// worker via a command channel and publishes results to `SharedAppState`.
pub struct AnalyzerConsumer {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl AnalyzerConsumer {
    /// Spawn the analyzer thread. `pipe` must be a SkipToLatest consumer
    /// connected to the pipeline.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        mut pipe: ConsumerPipe,
        state: Arc<SharedAppState>,
        config: AnalyzerConfig,
        commands: Receiver<AnalyzerCommand>,
        debug_recorder: Option<DebugRecorder>,
    ) -> Result<Self, String> {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);

        let mut analyzer = Analyzer::new();
        analyzer.set_ui_scaler(config.ui_scaler);
        analyzer.set_roi(
            config.pipeline_info.width as i32,
            config.pipeline_info.height as i32,
        );
        if let Some(path) = &config.calibration_path {
            if let Err(err) = analyzer.load_calibration(path) {
                log::warn!("analyzer consumer: failed to load initial calibration: {err}");
            }
        }

        let mut ctx = AnalyzerContext {
            analyzer,
            display_mode: config.display_mode,
            #[cfg(windows)]
            cursor_guard: config.cursor_guard,
            sample_index: 0,
            last_elapsed_frames: 0,
            last_total_frames: 0,
            last_cost_is_negative: false,
            last_recorded_frame_id: None,
            lap_start_frame: None,
            reset_pulse: 0,
            reset_kind: ResetKind::Manual,
            timer_reset_undo: TimerResetUndo::default(),
            debug_recorder,
            pipeline_info: config.pipeline_info,
            telemetry_stats: config.telemetry_stats,
            calibrating: false,
        };

        let handle = thread::Builder::new()
            .name("ruler-analyzer".to_string())
            .spawn(move || {
                log::info!("analyzer consumer started (SkipToLatest)");
                while running_clone.load(Ordering::Relaxed) {
                    // Drain any pending commands (non-blocking).
                    while let Ok(cmd) = commands.try_recv() {
                        match cmd {
                            AnalyzerCommand::LoadCalibration { path } => {
                                if let Err(err) = ctx.analyzer.load_calibration(&path) {
                                    log::error!("analyzer: load_calibration failed: {err}");
                                }
                                ctx.last_elapsed_frames = 0;
                                ctx.last_cost_is_negative = false;
                                ctx.last_recorded_frame_id = None;
                                ctx.timer_reset_undo.clear();
                                ctx.lap_start_frame = None;
                            }
                            AnalyzerCommand::ClearCalibration => {
                                // Unload the calibration so the analyzer goes
                                // silent (see the recv_frame guard below) and
                                // the worker-owned PreCalibration / Idle screen
                                // is not clobbered by stale Running results.
                                ctx.analyzer.clear_calibration();
                                ctx.analyzer.reset_timer();
                                ctx.last_elapsed_frames = 0;
                                ctx.last_cost_is_negative = false;
                                ctx.last_recorded_frame_id = None;
                                ctx.timer_reset_undo.clear();
                                ctx.lap_start_frame = None;
                            }
                            AnalyzerCommand::ResetTimer => {
                                if ctx.last_elapsed_frames != 0 {
                                    ctx.reset_pulse = ctx.reset_pulse.wrapping_add(1);
                                    ctx.reset_kind = ResetKind::Manual;
                                }
                                ctx.timer_reset_undo.remember_reset(ctx.last_elapsed_frames);
                                ctx.analyzer.reset_timer();
                                ctx.lap_start_frame = None;
                                ctx.last_elapsed_frames = 0;
                                ctx.last_recorded_frame_id = None;
                            }
                            AnalyzerCommand::UndoResetTimer => {
                                if let Some(elapsed) = ctx.timer_reset_undo.take() {
                                    ctx.analyzer.adjust_timer(elapsed - ctx.last_elapsed_frames);
                                    ctx.last_elapsed_frames = elapsed;
                                    ctx.lap_start_frame = None;
                                }
                            }
                            AnalyzerCommand::AdjustTimer { frames } => {
                                ctx.analyzer.adjust_timer(frames);
                                ctx.last_elapsed_frames += frames;
                            }
                            AnalyzerCommand::SetTimer { frames } => {
                                ctx.analyzer.adjust_timer(frames - ctx.last_elapsed_frames);
                                ctx.last_elapsed_frames = frames;
                            }
                            AnalyzerCommand::SetProfileIndex { index } => {
                                ctx.analyzer.set_profile_index(index);
                            }
                            AnalyzerCommand::SetDisplayMode(mode) => {
                                ctx.display_mode = mode;
                            }
                            AnalyzerCommand::SetRoi { width, height } => {
                                ctx.analyzer.set_roi(width, height);
                            }
                            AnalyzerCommand::ToggleLapTimer => {
                                ctx.lap_start_frame = if ctx.lap_start_frame.is_some() {
                                    None
                                } else {
                                    Some(ctx.last_elapsed_frames)
                                };
                            }
                            AnalyzerCommand::SetCalibrating { calibrating } => {
                                ctx.calibrating = calibrating;
                                if calibrating {
                                    ctx.last_recorded_frame_id = None;
                                }
                            }
                            AnalyzerCommand::Shutdown => {
                                running_clone.store(false, Ordering::Relaxed);
                                break;
                            }
                        }
                    }

                    match pipe.recv_frame() {
                        Ok(frame) => {
                            // Stay silent whenever the worker owns the UI:
                            // during an explicit calibration (the collector
                            // owns it) or whenever no calibration is loaded
                            // (PreCalibration / Idle / first run). Publishing
                            // here would clobber the worker-set OverlayMode on
                            // the very next frame.
                            if ctx.calibrating || !ctx.analyzer.has_calibration() {
                                continue;
                            }
                            analyze_and_publish(&state, &mut ctx, &frame);
                        }
                        Err(err) => {
                            log::debug!("analyzer: recv_frame failed: {err}");
                            break;
                        }
                    }
                }
                log::info!("analyzer consumer exiting");
            })
            .map_err(|e| format!("failed to spawn analyzer thread: {e}"))?;

        Ok(Self {
            running,
            handle: Some(handle),
        })
    }
}

impl Drop for AnalyzerConsumer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct AnalyzerContext {
    analyzer: Analyzer,
    display_mode: FrameDisplayMode,
    #[cfg(windows)]
    cursor_guard: Option<crate::pc_cursor_guard::SelfDrawnCursorGuard>,
    sample_index: u64,
    last_elapsed_frames: i32,
    last_total_frames: i32,
    last_cost_is_negative: bool,
    last_recorded_frame_id: Option<u64>,
    lap_start_frame: Option<i32>,
    reset_pulse: u32,
    reset_kind: ResetKind,
    timer_reset_undo: TimerResetUndo,
    debug_recorder: Option<DebugRecorder>,
    pipeline_info: PipelineInfo,
    telemetry_stats: Arc<RunTelemetryStats>,
    calibrating: bool,
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

fn analyze_and_publish(state: &SharedAppState, ctx: &mut AnalyzerContext, frame: &PipelineFrame) {
    ctx.sample_index += 1;
    let capture_dur_us = frame.capture_duration_us;

    // Debug recording: write the video frame first (before analysis) so
    // ffmpeg's wallclock timestamps track capture timing.
    if let Some(ref mut recorder) = ctx.debug_recorder {
        recorder.record_pipeline_frame(frame);
    }

    let battle_state = scanner::detect_battle_state_with_ui_scaler(
        &frame.data,
        frame.width,
        frame.height,
        frame.format,
        ctx.analyzer.ui_scaler(),
    );

    // Cursor guard check (Windows-only, uses pipeline_info.window_info).
    #[cfg(windows)]
    {
        if cursor_blocks_cost_bar(
            &ctx.pipeline_info,
            &mut ctx.cursor_guard,
            ctx.analyzer.roi(),
            battle_state,
        ) {
            publish_cursor_blocked(state, ctx, frame, battle_state);
            state.update_timing(WorkerTimingSnapshot {
                sample_index: ctx.sample_index,
            });
            return;
        }
    }

    ctx.telemetry_stats.record_analyzed_frame();
    match ctx.analyzer.analyze_raw_buffer(
        frame.data.as_slice(),
        frame.width,
        frame.height,
        frame.format,
    ) {
        Ok(result) => {
            // Debug CSV row.
            if let Some(ref mut recorder) = ctx.debug_recorder {
                recorder.record_analysis_row(&result, capture_dur_us as u128);
            }

            log::trace!(
                "analyzer frame {} => battle_state={}, logical_frame={:?}, total={}, elapsed={}",
                ctx.sample_index,
                result.battle_state.as_str(),
                result.logical_frame,
                result.total_frames_in_cycle,
                result.elapsed_frames
            );

            let auto_reset = result.battle_state == BattleState::BattleBegin
                && ctx.last_elapsed_frames != result.elapsed_frames;
            if auto_reset {
                state.clear_api_frame_history();
                ctx.last_recorded_frame_id = None;
                ctx.telemetry_stats.record_action_restart();
            }

            ctx.last_total_frames = result.total_frames_in_cycle;
            ctx.last_cost_is_negative = result.cost_is_negative;
            if result.battle_state == BattleState::BattleBegin {
                if ctx.last_elapsed_frames != result.elapsed_frames {
                    ctx.timer_reset_undo.remember_reset(ctx.last_elapsed_frames);
                    if ctx.last_elapsed_frames != 0 {
                        ctx.reset_pulse = ctx.reset_pulse.wrapping_add(1);
                        ctx.reset_kind = ResetKind::Auto;
                    }
                }
                ctx.last_elapsed_frames = result.elapsed_frames;
                ctx.lap_start_frame = None;
            } else if result.logical_frame.is_some() {
                ctx.last_elapsed_frames = result.elapsed_frames;
            }

            let record = api_frame_record(state, ctx, frame, &result);
            publish_running(state, ctx, &record);
            state.record_api_frame(record);
            ctx.last_recorded_frame_id = Some(frame.id);
            state.update_timing(WorkerTimingSnapshot {
                sample_index: ctx.sample_index,
            });
        }
        Err(err) => {
            publish_error(state, format!("analyze error: {err}"));
        }
    }
}

#[cfg(windows)]
fn cursor_blocks_cost_bar(
    pipeline_info: &PipelineInfo,
    cursor_guard: &mut Option<crate::pc_cursor_guard::SelfDrawnCursorGuard>,
    roi: Option<ruler_core::analysis::roi::Roi>,
    battle_state: BattleState,
) -> bool {
    // The cursor guard detects when the in-game self-drawn cursor overlaps
    // the cost bar ROI and asks the consumer to skip that frame so the timer
    // is not corrupted by an occluded capture.
    let Some(guard) = cursor_guard.as_mut() else {
        return false;
    };
    guard.should_pause_for_frame(pipeline_info.window_info, roi, battle_state)
}

fn publish_running(state: &SharedAppState, ctx: &AnalyzerContext, record: &ApiFrameRecord) {
    let display_frame = ctx.display_mode.display_frame(record.current_frame);
    let display_total = if record.total_frames_in_cycle > 0 {
        display_total_with_cost_marker(
            ctx.display_mode,
            record.total_frames_in_cycle,
            record.cost_is_negative,
        )
    } else {
        "/--".to_string()
    };
    let lap_frames = ctx
        .lap_start_frame
        .map(|start| ctx.last_elapsed_frames - start);
    let reset_pulse = ctx.reset_pulse;
    let reset_kind = ctx.reset_kind;
    let can_undo_reset = ctx.timer_reset_undo.is_available();
    state.update_ui(|ui, api| {
        ui.mode = OverlayMode::Running;
        ui.message.clear();
        ui.display_mode = ctx.display_mode;
        ui.display_frame = display_frame;
        ui.display_total = display_total;
        ui.time_str = format_time_from_frames(ctx.last_elapsed_frames);
        ui.lap_frames = lap_frames;
        ui.can_undo_reset = can_undo_reset;
        ui.cursor_blocked = false;
        ui.total_frames_in_cycle = record.total_frames_in_cycle;
        ui.reset_pulse = reset_pulse;
        ui.reset_kind = reset_kind;
        api.update_from_frame_record(record);
    });
}

fn publish_cursor_blocked(
    state: &SharedAppState,
    ctx: &AnalyzerContext,
    frame: &PipelineFrame,
    battle_state: BattleState,
) {
    let lap_frames = ctx
        .lap_start_frame
        .map(|start| ctx.last_elapsed_frames - start);
    let dropped_since_previous = dropped_since_previous(ctx.last_recorded_frame_id, frame.id);
    state.update_ui(|ui, api| {
        ui.mode = OverlayMode::Running;
        ui.message.clear();
        ui.progress_percent = 0.0;
        ui.cursor_blocked = true;
        ui.time_str = format_time_from_frames(ctx.last_elapsed_frames);
        ui.lap_frames = lap_frames;
        api.is_running = false;
        api.current_frame = None;
        api.total_frames_in_cycle = 0;
        api.total_elapsed_frames = ctx.last_elapsed_frames;
        api.frame_id = Some(frame.id);
        api.sample_index = ctx.sample_index;
        api.dropped_since_previous = dropped_since_previous;
        api.raw_pixel_width = None;
        api.cost_is_negative = false;
        api.battle_state = Some(battle_state.as_str().to_string());
        api.capture_width = Some(frame.width);
        api.capture_height = Some(frame.height);
        api.capture_format = Some(pixel_format_name(frame.format).to_string());
        api.capture_timestamp_ns = Some(frame.capture_timestamp_ns);
        api.capture_duration_us = Some(frame.capture_duration_us);
        api.timing_debug = None;
    });
}

fn api_frame_record(
    state: &SharedAppState,
    ctx: &AnalyzerContext,
    frame: &PipelineFrame,
    result: &ruler_core::engine::FrameResult,
) -> ApiFrameRecord {
    ApiFrameRecord {
        frame_id: frame.id,
        sample_index: ctx.sample_index,
        dropped_since_previous: dropped_since_previous(ctx.last_recorded_frame_id, frame.id),
        is_running: result.logical_frame.is_some(),
        current_frame: result.logical_frame,
        total_frames_in_cycle: result.total_frames_in_cycle,
        total_elapsed_frames: ctx.last_elapsed_frames,
        active_profile: state.snapshot().api.active_profile,
        raw_pixel_width: result.raw_pixel_width,
        cost_is_negative: result.cost_is_negative,
        battle_state: result.battle_state.as_str().to_string(),
        capture_width: frame.width,
        capture_height: frame.height,
        capture_format: pixel_format_name(frame.format).to_string(),
        capture_timestamp_ns: frame.capture_timestamp_ns,
        capture_duration_us: frame.capture_duration_us,
        timing_debug: result.timing_debug,
    }
}

fn dropped_since_previous(previous_frame_id: Option<u64>, frame_id: u64) -> u64 {
    previous_frame_id.map_or(0, |previous| {
        frame_id.saturating_sub(previous.saturating_add(1))
    })
}

fn pixel_format_name(format: PixelFormat) -> &'static str {
    match format {
        PixelFormat::Rgba => "rgba",
        PixelFormat::Bgr => "bgr",
        PixelFormat::Bgra => "bgra",
    }
}

fn publish_error(state: &SharedAppState, error: String) {
    log::error!("{error}");
    state.update_ui(|ui, api| {
        ui.mode = OverlayMode::Error;
        ui.message = error;
        ui.progress_percent = 0.0;
        ui.cursor_blocked = false;
        api.is_running = false;
        api.current_frame = None;
    });
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

// Unused import suppressors for non-Windows builds.
#[allow(dead_code)]
fn _suppress_unused() {
    let _ = PixelFormat::Rgba;
    let _: Option<Instant> = None;
    let _: Option<ApiStateSnapshot> = None;
}
