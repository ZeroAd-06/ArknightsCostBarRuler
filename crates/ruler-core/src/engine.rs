use std::path::Path;

use crate::analysis::calibration::{CalibrationTimingModel, LoadedCalibration};
use crate::analysis::roi::{self, Roi};
use crate::analysis::scanner::{self, BattleState, PixelFormat};
use crate::analysis::synthesis::synthesized_width_for_phase;
use crate::capture::CapturedFrame;
use crate::fp24::{Fp24, Fp24CostTiming};

mod timing;
use timing::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameResult {
    pub logical_frame: Option<i32>,
    pub total_frames_in_cycle: i32,
    pub raw_pixel_width: Option<i32>,
    pub elapsed_frames: i32,
    pub cost_is_negative: bool,
    pub battle_state: BattleState,
    pub timing_debug: Option<TimingDebug>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimingDebug {
    pub required_fp: i64,
    pub speed_fp: i64,
    pub accumulator_fp: i64,
    pub advanced_frames: i32,
    pub frames_since_cycle_start: i32,
    pub frames_until_next_cost: i32,
    pub match_error_px: i32,
}

/// Layer 2 analyzer — holds calibration, ROI, and timing state. Does NOT
/// own a capture backend; that responsibility belongs to Layer 1
/// ([`crate::pipeline::CapturePipeline`]).
pub struct Analyzer {
    calibration: Option<LoadedCalibration>,
    roi: Option<Roi>,
    bar_width_frac: Option<f64>,
    ui_scaler: f64,
    current_profile_index: usize,
    elapsed_frames: f64,
    fp24_timing: Option<Fp24CostTiming>,
    fp24_anchor_initialized: bool,
    last_capture_timestamp_ns: Option<u64>,
    timestamp_frame_residual: f64,
    last_known_total_frames: i32,
    last_known_cycle_total_frames: i32,
    last_known_cost_is_negative: bool,
    /// Consecutive analysed frames spent out of an active battle. A genuine
    /// pre-battle banner only appears after a sustained out-of-battle stretch
    /// (loading / settlement), so this counter separates it from the brief
    /// `BeforeOrAfterBattle` flicker of a mid-battle overlay (deployment slow-mo,
    /// pause/settings menu). Reset to zero the instant a battle is in progress.
    out_of_battle_frames: u32,
    /// A `BattleBegin` title screen can span many frames. Arm this after seeing
    /// an active battle so the title screen resets the timer once, while still
    /// letting the user undo that reset before the next battle begins.
    battle_begin_reset_armed: bool,
    /// After an automatic `BattleBegin` reset, the first readable in-battle cost
    /// bar may already be a few frames into the cycle. Count that first phase
    /// once so entering battle does not lose the frames before the first sample.
    pending_battle_start_phase: bool,
    last_reported_battle_state: Option<BattleState>,
}

#[derive(Clone, Copy, Debug)]
struct TimingCandidate {
    timing: Fp24CostTiming,
    advanced_frames: i32,
    total_frames_in_cycle: i32,
    frame_error: f64,
    timestamp_error: f64,
    pixel_error: i32,
    debug: TimingDebug,
}

impl TimingCandidate {
    fn is_better_than(self, other: Self) -> bool {
        self.frame_error < other.frame_error - f64::EPSILON
            || ((self.frame_error - other.frame_error).abs() <= f64::EPSILON
                && self.timestamp_error < other.timestamp_error - f64::EPSILON)
            || ((self.frame_error - other.frame_error).abs() <= f64::EPSILON
                && (self.timestamp_error - other.timestamp_error).abs() <= f64::EPSILON
                && self.pixel_error < other.pixel_error)
            || ((self.frame_error - other.frame_error).abs() <= f64::EPSILON
                && (self.timestamp_error - other.timestamp_error).abs() <= f64::EPSILON
                && self.pixel_error == other.pixel_error
                && self.advanced_frames < other.advanced_frames)
    }
}

/// Minimum consecutive out-of-battle frames before a `BeforeOrAfterBattle` frame
/// is trusted as a genuine pre-battle banner (rather than a brief mid-battle
/// overlay). Observed overlay flickers last only a handful of frames, while real
/// loading/settlement stretches run into the dozens-to-hundreds.
const PRE_BATTLE_BANNER_MIN_OUT_FRAMES: u32 = 30;
const MAX_MATCH_ADVANCE_FRAMES: i32 = 180;

impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer {
    pub fn new() -> Self {
        Self {
            calibration: None,
            roi: None,
            bar_width_frac: None,
            ui_scaler: roi::DEFAULT_UI_SCALER,
            current_profile_index: 0,
            elapsed_frames: 0.0,
            fp24_timing: None,
            fp24_anchor_initialized: false,
            last_capture_timestamp_ns: None,
            timestamp_frame_residual: 0.0,
            last_known_total_frames: 0,
            last_known_cycle_total_frames: 0,
            last_known_cost_is_negative: false,
            out_of_battle_frames: 0,
            battle_begin_reset_armed: true,
            pending_battle_start_phase: false,
            last_reported_battle_state: None,
        }
    }

    pub fn load_calibration<P: AsRef<Path>>(&mut self, path: P) -> Result<(), String> {
        log::info!("loading calibration from '{}'", path.as_ref().display());
        let (total_bar_width, bar_width_frac) = self.calibration_geometry()?;
        let loaded = LoadedCalibration::from_file(path.as_ref(), total_bar_width, bar_width_frac)?;
        self.set_loaded_calibration(loaded);
        Ok(())
    }

    pub fn load_calibration_json(&mut self, json: &str) -> Result<(), String> {
        let (total_bar_width, bar_width_frac) = self.calibration_geometry()?;
        let loaded = LoadedCalibration::from_json(json, total_bar_width, bar_width_frac)?;
        self.set_loaded_calibration(loaded);
        Ok(())
    }

    /// Unload the current calibration. After this, [`Self::analyze_captured_frame`]
    /// returns `Err("No calibration loaded")` until a new profile is loaded.
    pub fn clear_calibration(&mut self) {
        self.calibration = None;
    }

    /// Whether a calibration profile is currently loaded.
    pub fn has_calibration(&self) -> bool {
        self.calibration.is_some()
    }

    pub fn analyze_captured_frame(
        &mut self,
        frame_data: &CapturedFrame,
    ) -> Result<FrameResult, String> {
        self.analyze_captured_frame_at(frame_data, None)
    }

    pub fn analyze_captured_frame_at(
        &mut self,
        frame_data: &CapturedFrame,
        capture_timestamp_ns: Option<u64>,
    ) -> Result<FrameResult, String> {
        self.analyze_frame(
            &frame_data.data,
            frame_data.width,
            frame_data.height,
            frame_data.format,
            capture_timestamp_ns,
        )
    }

    pub fn analyze_raw_buffer(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<FrameResult, String> {
        self.analyze_raw_buffer_with_timestamp(buffer, width, height, format, None)
    }

    pub fn analyze_raw_buffer_with_timestamp(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
        capture_timestamp_ns: Option<u64>,
    ) -> Result<FrameResult, String> {
        self.analyze_frame(buffer, width, height, format, capture_timestamp_ns)
    }

    pub fn set_roi(&mut self, screen_width: i32, screen_height: i32) {
        self.roi = Some(roi::find_cost_bar_roi_with_ui_scaler(
            screen_width,
            screen_height,
            self.ui_scaler,
        ));
        self.bar_width_frac = Some(roi::cost_bar_width_frac_with_ui_scaler(
            screen_width,
            screen_height,
            self.ui_scaler,
        ));
    }

    pub fn set_roi_value(&mut self, roi: Roi) {
        self.bar_width_frac = Some((roi.1 - roi.0) as f64);
        self.roi = Some(roi);
    }

    pub fn set_ui_scaler(&mut self, ui_scaler: f64) {
        self.ui_scaler = normalized_ui_scaler(ui_scaler);
    }

    pub fn ui_scaler(&self) -> f64 {
        self.ui_scaler
    }

    pub fn roi(&self) -> Option<Roi> {
        self.roi
    }

    pub fn reset_timer(&mut self) {
        log::debug!("resetting engine timer");
        self.elapsed_frames = 0.0;
        self.reset_fp24_timing();
        self.last_known_total_frames = 0;
        self.last_known_cycle_total_frames = 0;
        self.last_known_cost_is_negative = false;
        self.pending_battle_start_phase = false;
    }

    pub fn adjust_timer(&mut self, frames: i32) {
        self.elapsed_frames += frames as f64;
        self.last_known_total_frames = rounded_frame_count(self.elapsed_frames);
    }

    pub fn set_profile_index(&mut self, index: usize) {
        if let Some(cal) = &self.calibration {
            if index < cal.tables.len() {
                self.current_profile_index = index;
                self.reset_fp24_timing();
            }
        }
    }

    fn set_loaded_calibration(&mut self, loaded: LoadedCalibration) {
        self.calibration = Some(loaded);
        self.current_profile_index = 0;
        self.reset_fp24_timing();
        self.last_known_cycle_total_frames = 0;
        self.last_known_cost_is_negative = false;
        self.last_known_total_frames = rounded_frame_count(self.elapsed_frames);
        self.out_of_battle_frames = 0;
        self.battle_begin_reset_armed = true;
        self.pending_battle_start_phase = false;
        self.last_reported_battle_state = None;
    }

    fn calibration_geometry(&self) -> Result<(i32, f64), String> {
        let roi = self
            .roi
            .ok_or_else(|| "No ROI set - call set_roi() before loading calibration".to_string())?;
        let total_bar_width = roi.1 - roi.0;
        if total_bar_width <= 0 {
            return Err("No valid ROI width set before loading calibration".to_string());
        }
        Ok((
            total_bar_width,
            self.bar_width_frac.unwrap_or(total_bar_width as f64),
        ))
    }

    fn reset_fp24_timing(&mut self) {
        self.fp24_timing = self.calibration.as_ref().map(|calibration| {
            let CalibrationTimingModel::Fp24AccumulatorV1 { required } = calibration.timing_model;
            Fp24CostTiming::new(required)
        });
        self.fp24_anchor_initialized = false;
        self.last_capture_timestamp_ns = None;
        self.timestamp_frame_residual = 0.0;
    }

    const fn speed_for_cost_state(cost_is_negative: bool) -> Fp24 {
        if cost_is_negative {
            Fp24::NEGATIVE_SPEED
        } else {
            Fp24::NORMAL_SPEED
        }
    }

    fn timestamp_prior_frames(&self, capture_timestamp_ns: Option<u64>) -> Option<f64> {
        let current = capture_timestamp_ns?;
        let previous = self.last_capture_timestamp_ns?;
        if current < previous {
            return None;
        }
        let elapsed_ns = current - previous;
        Some(elapsed_ns as f64 * 30.0 / 1_000_000_000.0 + self.timestamp_frame_residual)
    }

    fn update_timestamp_residual(
        &mut self,
        capture_timestamp_ns: Option<u64>,
        advanced_frames: i32,
    ) {
        self.timestamp_frame_residual = self
            .timestamp_prior_frames(capture_timestamp_ns)
            .map_or(0.0, |prior| {
                (prior - advanced_frames as f64).clamp(-2.0, 2.0)
            });
    }

    fn select_timing_candidate(
        &self,
        calibration: &LoadedCalibration,
        base_profile: usize,
        pixel_width: i32,
        cost_is_negative: bool,
        capture_timestamp_ns: Option<u64>,
    ) -> Option<TimingCandidate> {
        let CalibrationTimingModel::Fp24AccumulatorV1 { required } = calibration.timing_model;
        let base_timing = self
            .fp24_timing
            .unwrap_or_else(|| Fp24CostTiming::new(required));
        let speed = Self::speed_for_cost_state(cost_is_negative);
        let advance_speed = if self.fp24_anchor_initialized
            && cost_is_negative
            && !self.last_known_cost_is_negative
        {
            Self::speed_for_cost_state(self.last_known_cost_is_negative)
        } else {
            speed
        };
        let timestamp_prior = self.timestamp_prior_frames(capture_timestamp_ns);
        let max_advance = timestamp_prior.map_or(MAX_MATCH_ADVANCE_FRAMES, |prior| {
            ((prior.ceil() as i32).saturating_add(30)).clamp(0, MAX_MATCH_ADVANCE_FRAMES)
        });
        let mut best: Option<TimingCandidate> = None;

        for advanced_frames in 0..=max_advance {
            let timing = base_timing.advanced(advanced_frames, advance_speed);
            if timing.cycle_index() > base_timing.cycle_index()
                && base_timing.phase() < 0.75
                && timestamp_prior.is_none_or(|prior| prior < 15.0)
            {
                continue;
            }
            let profile_index =
                (base_profile + timing.cycle_index() as usize) % calibration.tables.len();
            let table = &calibration.tables[profile_index];
            let observed_frame =
                table.lookup_display_frame(pixel_width, calibration.total_bar_width)?;
            let simulated_frame = timing.phase() * table.total_frames as f64;
            let frame_error = (observed_frame - simulated_frame).abs();
            let expected_width = synthesized_width_for_phase(
                calibration.total_bar_width,
                calibration.bar_width_frac,
                timing.phase(),
            );
            let pixel_error = (expected_width - pixel_width).abs();
            let timestamp_error =
                timestamp_prior.map_or(0.0, |prior| (advanced_frames as f64 - prior).abs());
            let frames_until_next_cost = timing.frames_until_next_cost(speed);
            let total_frames_in_cycle = timing.total_frames_in_cycle(speed);
            let debug = TimingDebug {
                required_fp: timing.required().raw(),
                speed_fp: speed.raw(),
                accumulator_fp: timing.accumulator().raw(),
                advanced_frames,
                frames_since_cycle_start: timing.frames_since_cycle_start(),
                frames_until_next_cost,
                match_error_px: pixel_error,
            };
            let candidate = TimingCandidate {
                timing,
                advanced_frames,
                total_frames_in_cycle,
                frame_error,
                timestamp_error,
                pixel_error,
                debug,
            };

            if best.map_or(true, |current| candidate.is_better_than(current)) {
                best = Some(candidate);
            }
        }

        best
    }

    fn analyze_frame(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
        capture_timestamp_ns: Option<u64>,
    ) -> Result<FrameResult, String> {
        let battle_state = scanner::detect_battle_state_with_ui_scaler(
            buffer,
            width,
            height,
            format,
            self.ui_scaler,
        );
        self.analyze_frame_with_battle_state_at(
            buffer,
            width,
            height,
            format,
            battle_state,
            capture_timestamp_ns,
        )
    }

    #[cfg(test)]
    fn analyze_frame_with_battle_state(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
        battle_state: BattleState,
    ) -> Result<FrameResult, String> {
        self.analyze_frame_with_battle_state_at(buffer, width, height, format, battle_state, None)
    }

    fn analyze_frame_with_battle_state_at(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
        battle_state: BattleState,
        capture_timestamp_ns: Option<u64>,
    ) -> Result<FrameResult, String> {
        let battle_state = self.apply_battle_state_context(battle_state);
        if self.last_reported_battle_state != Some(battle_state) {
            log::debug!("battle state -> {}", battle_state.as_str());
            self.last_reported_battle_state = Some(battle_state);
        }

        if battle_state.is_in_battle() {
            self.battle_begin_reset_armed = true;
        } else if battle_state == BattleState::BattleBegin && self.battle_begin_reset_armed {
            self.reset_timer();
            self.battle_begin_reset_armed = false;
            self.pending_battle_start_phase = true;
        }

        if self.calibration.is_none() {
            return Err("No calibration loaded".to_string());
        }
        let roi = self.roi.ok_or_else(|| {
            "No ROI set - call connect() first or use set_roi()/set_roi_value()".to_string()
        })?;

        if !battle_state.is_in_battle() {
            if battle_state == BattleState::BeforeOrAfterBattle {
                self.reset_fp24_timing();
            }
            let result = FrameResult {
                logical_frame: None,
                total_frames_in_cycle: self.last_known_cycle_total_frames,
                raw_pixel_width: None,
                elapsed_frames: self.last_known_total_frames,
                cost_is_negative: self.last_known_cost_is_negative,
                battle_state,
                timing_debug: None,
            };
            log::trace!(
                "frame summary: battle_state={}, logical_frame={:?}, total={}, raw_width={:?}, elapsed={}, negative={}",
                result.battle_state.as_str(),
                result.logical_frame,
                result.total_frames_in_cycle,
                result.raw_pixel_width,
                result.elapsed_frames,
                result.cost_is_negative
            );
            self.last_capture_timestamp_ns = None;
            self.timestamp_frame_residual = 0.0;
            return Ok(result);
        }

        let calibration = self
            .calibration
            .as_ref()
            .ok_or_else(|| "No calibration loaded".to_string())?;

        let pixel_width = scanner::get_raw_filled_pixel_width(buffer, width, height, format, roi);
        let cost_is_negative =
            scanner::is_cost_negative_with_ui_scaler(buffer, width, height, format, self.ui_scaler);

        let num_profiles = calibration.tables.len();
        let base_profile = if num_profiles == 0 {
            0
        } else {
            self.current_profile_index.min(num_profiles - 1)
        };

        let (logical_frame, total_frames_in_cycle, timing_debug) = if num_profiles > 0 {
            if let Some(pixel_width) = pixel_width {
                if let Some(candidate) = self.select_timing_candidate(
                    calibration,
                    base_profile,
                    pixel_width,
                    cost_is_negative,
                    capture_timestamp_ns,
                ) {
                    let should_count_elapsed =
                        self.fp24_anchor_initialized || self.pending_battle_start_phase;
                    if should_count_elapsed {
                        self.elapsed_frames += candidate.advanced_frames as f64;
                        self.last_known_total_frames = rounded_frame_count(self.elapsed_frames);
                    }
                    self.fp24_anchor_initialized = true;
                    self.pending_battle_start_phase = false;
                    self.fp24_timing = Some(candidate.timing);
                    self.update_timestamp_residual(capture_timestamp_ns, candidate.advanced_frames);
                    self.last_capture_timestamp_ns = capture_timestamp_ns;
                    (
                        Some(candidate.timing.frames_since_cycle_start()),
                        candidate.total_frames_in_cycle,
                        Some(candidate.debug),
                    )
                } else {
                    (None, self.last_known_cycle_total_frames, None)
                }
            } else {
                (None, self.last_known_cycle_total_frames, None)
            }
        } else {
            self.fp24_timing = None;
            self.fp24_anchor_initialized = false;
            (None, 0, None)
        };

        self.last_known_cycle_total_frames = total_frames_in_cycle;
        self.last_known_cost_is_negative = cost_is_negative;

        let result = FrameResult {
            logical_frame,
            total_frames_in_cycle,
            raw_pixel_width: pixel_width,
            elapsed_frames: self.last_known_total_frames,
            cost_is_negative,
            battle_state,
            timing_debug,
        };
        log::trace!(
            "frame summary: battle_state={}, logical_frame={:?}, total={}, raw_width={:?}, elapsed={}, negative={}",
            result.battle_state.as_str(),
            result.logical_frame,
            result.total_frames_in_cycle,
            result.raw_pixel_width,
            result.elapsed_frames,
            result.cost_is_negative
        );
        Ok(result)
    }

    fn apply_battle_state_context(&mut self, battle_state: BattleState) -> BattleState {
        if battle_state.is_in_battle() {
            self.out_of_battle_frames = 0;
            return battle_state;
        }

        self.out_of_battle_frames = self.out_of_battle_frames.saturating_add(1);

        // A genuine pre-battle banner only shows after a sustained out-of-battle
        // stretch (loading / settlement). A `BeforeOrAfterBattle` that appears
        // within a few frames of leaving a battle is a transient overlay
        // (deployment slow-mo, pause/settings menu); report it as `NotInBattle`.
        if battle_state == BattleState::BeforeOrAfterBattle
            && self.out_of_battle_frames < PRE_BATTLE_BANNER_MIN_OUT_FRAMES
        {
            return BattleState::NotInBattle;
        }

        battle_state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::calibration::{
        CalibrationData, CalibrationTimingModel, ProfileData, TIMING_MODEL_FP24_ACCUMULATOR_V1,
    };
    use crate::analysis::mapping::CalibrationTable;
    use crate::fp24::Fp24;
    use std::collections::HashMap;

    const TEST_SCREEN_WIDTH: u32 = 1280;
    const TEST_SCREEN_HEIGHT: u32 = 720;
    const TEST_ROI: Roi = (100, 140, 100);

    #[test]
    fn analyze_raw_buffer_tracks_frames() {
        let mut engine = Analyzer::new();
        engine.set_roi_value((0, 20, 0));
        engine
            .load_calibration_json(
                r#"{
                    "format_version": 4,
                    "timing_model": "fp24_accumulator_v1",
                    "required_fp": 16777216,
                    "profiles": [{"total_frames": 30}]
                }"#,
            )
            .unwrap();

        let mut buffer = vec![30u8; 20 * 3];
        buffer[0] = 252;
        buffer[1] = 252;
        buffer[2] = 252;
        buffer[3] = 252;
        buffer[4] = 252;
        buffer[5] = 252;

        let result = engine
            .analyze_frame_with_battle_state(
                &buffer,
                20,
                1,
                PixelFormat::Bgr,
                BattleState::OneXRunning,
            )
            .unwrap();

        assert_eq!(result.raw_pixel_width, Some(2));
        assert_eq!(result.logical_frame, Some(1));
        // First observed in-battle frame only sets the phase anchor (elapsed 0).
        assert_eq!(result.elapsed_frames, 0);
        assert!(!result.cost_is_negative);
    }

    #[test]
    fn normal_cycle_elapsed_frames_match_existing_behavior() {
        let mut engine = engine_with_profiles(&[30]);

        let result = analyze_width(&mut engine, 0, false);
        assert_eq!(result.logical_frame, Some(0));
        assert_eq!(result.total_frames_in_cycle, 30);
        assert_eq!(result.elapsed_frames, 0);

        let result = analyze_width(&mut engine, 15, false);
        assert_eq!(result.logical_frame, Some(15));
        assert_eq!(result.total_frames_in_cycle, 30);
        assert_eq!(result.elapsed_frames, 15);

        let result = analyze_width(&mut engine, 29, false);
        assert_eq!(result.logical_frame, Some(29));
        assert_eq!(result.elapsed_frames, 29);

        let result = analyze_width(&mut engine, 0, false);
        assert_eq!(result.logical_frame, Some(0));
        assert_eq!(result.total_frames_in_cycle, 30);
        assert_eq!(result.elapsed_frames, 30);
    }

    #[test]
    fn entering_negative_cost_at_same_phase_does_not_jump_elapsed_time() {
        let mut engine = engine_with_profiles(&[30]);

        analyze_width(&mut engine, 0, false);
        analyze_width(&mut engine, 15, false);
        let result = analyze_width(&mut engine, 15, true);

        assert_eq!(result.logical_frame, Some(15));
        assert_eq!(result.total_frames_in_cycle, 45);
        assert_eq!(result.elapsed_frames, 15);
        assert!(result.cost_is_negative);
        assert_eq!(
            result.timing_debug.map(|debug| debug.speed_fp),
            Some(Fp24::NEGATIVE_SPEED.raw())
        );
    }

    #[test]
    fn full_negative_cost_cycle_counts_double_frames() {
        let mut engine = engine_with_profiles(&[30]);

        analyze_width(&mut engine, 0, true);
        for width in 1..30 {
            analyze_width(&mut engine, width, true);
        }
        let result = analyze_width(&mut engine, 0, false);

        assert_eq!(result.logical_frame, Some(0));
        assert_eq!(result.total_frames_in_cycle, 30);
        assert_eq!(result.elapsed_frames, 59);
        assert!(!result.cost_is_negative);
    }

    #[test]
    fn entering_negative_cost_from_normal_state_keeps_elapsed_cycle_frames_unscaled() {
        let mut engine = engine_with_profiles(&[30]);

        analyze_width(&mut engine, 0, false);
        let result = analyze_width(&mut engine, 15, true);

        assert_eq!(result.logical_frame, Some(15));
        assert_eq!(result.total_frames_in_cycle, 45);
        assert_eq!(result.elapsed_frames, 15);
        assert!(result.cost_is_negative);
        assert_eq!(
            result
                .timing_debug
                .map(|debug| (debug.frames_since_cycle_start, debug.frames_until_next_cost)),
            Some((15, 30))
        );
    }

    #[test]
    fn negative_cost_interpolates_widths_missing_from_positive_profile() {
        let mut engine = Analyzer::new();
        engine.set_roi_value(TEST_ROI);
        engine.set_loaded_calibration(loaded_calibration_from_maps(
            100,
            &[(8, &[(0, 0), (3, 2), (5, 4), (7, 7)] as &[(i32, i32)])],
        ));

        let result = analyze_width(&mut engine, 0, true);
        assert_eq!(result.logical_frame, Some(0));
        assert_eq!(result.total_frames_in_cycle, 16);
        assert_eq!(result.elapsed_frames, 0);

        let result = analyze_width(&mut engine, 1, true);
        assert_eq!(result.logical_frame, Some(1));
        assert_eq!(result.total_frames_in_cycle, 16);
        assert_eq!(result.elapsed_frames, 1);

        let result = analyze_width(&mut engine, 4, true);
        assert_eq!(result.logical_frame, Some(6));
        assert_eq!(result.elapsed_frames, 6);

        let result = analyze_width(&mut engine, 6, true);
        assert_eq!(result.logical_frame, Some(11));
        assert_eq!(result.elapsed_frames, 11);
    }

    #[test]
    fn alternating_profiles_follow_required_value_and_negative_cost_halves_speed() {
        let mut engine = engine_with_profiles(&[38, 37]);

        analyze_width_at(&mut engine, 0, false, 0);
        analyze_width_at(&mut engine, 37, false, 37);
        let result = analyze_width_at(&mut engine, 0, false, 38);
        assert_eq!(result.logical_frame, Some(0));
        assert_eq!(result.total_frames_in_cycle, 37);
        assert_eq!(result.elapsed_frames, 38);

        let result = analyze_width_at(&mut engine, 10, true, 48);
        assert_eq!(
            result.timing_debug.map(|debug| debug.speed_fp),
            Some(Fp24::NEGATIVE_SPEED.raw())
        );
        assert!(result.total_frames_in_cycle > 38);
        assert!(result.elapsed_frames > 38);

        let result = analyze_width_at(&mut engine, 11, true, 50);
        assert_eq!(
            result.timing_debug.map(|debug| debug.speed_fp),
            Some(Fp24::NEGATIVE_SPEED.raw())
        );
        assert!(result.elapsed_frames > 38);
    }

    #[test]
    fn not_in_battle_hides_frame_and_preserves_phase_anchor() {
        let mut engine = engine_with_profiles(&[30]);

        analyze_width(&mut engine, 0, false);
        let result = analyze_width(&mut engine, 8, false);
        assert_eq!(result.logical_frame, Some(8));
        assert_eq!(result.elapsed_frames, 8);

        let result = analyze_width_with_state(&mut engine, 0, false, BattleState::NotInBattle);
        assert_eq!(result.logical_frame, None);
        assert_eq!(result.raw_pixel_width, None);
        assert_eq!(result.total_frames_in_cycle, 30);
        assert_eq!(result.elapsed_frames, 8);

        let result = analyze_width(&mut engine, 8, false);
        assert_eq!(result.logical_frame, Some(8));
        assert_eq!(result.total_frames_in_cycle, 30);
        assert_eq!(result.elapsed_frames, 8);
    }

    #[test]
    fn pre_battle_banner_keeps_elapsed_time_until_battle_begin() {
        let mut engine = engine_with_profiles(&[30]);

        // A battle runs, ends into settlement/menu, then the next battle's
        // pre-battle banner appears only after a sustained out-of-battle span.
        analyze_width(&mut engine, 0, false);
        let result = analyze_width(&mut engine, 20, false);
        assert_eq!(result.elapsed_frames, 20);

        for _ in 0..PRE_BATTLE_BANNER_MIN_OUT_FRAMES {
            let result = analyze_width_with_state(&mut engine, 0, false, BattleState::NotInBattle);
            assert_eq!(result.elapsed_frames, 20);
        }

        let result =
            analyze_width_with_state(&mut engine, 0, false, BattleState::BeforeOrAfterBattle);
        assert_eq!(result.battle_state, BattleState::BeforeOrAfterBattle);
        assert_eq!(result.logical_frame, None);
        assert_eq!(result.total_frames_in_cycle, 30);
        assert_eq!(result.elapsed_frames, 20);

        let result = analyze_width(&mut engine, 5, false);
        assert_eq!(result.logical_frame, Some(5));
        assert_eq!(result.total_frames_in_cycle, 30);
        assert_eq!(result.elapsed_frames, 20);
    }

    #[test]
    fn battle_begin_resets_elapsed_time_for_next_battle() {
        let mut engine = engine_with_profiles(&[30]);

        analyze_width(&mut engine, 0, false);
        let result = analyze_width(&mut engine, 20, false);
        assert_eq!(result.elapsed_frames, 20);

        let result = analyze_width_with_state(&mut engine, 0, false, BattleState::BattleBegin);
        assert_eq!(result.battle_state, BattleState::BattleBegin);
        assert_eq!(result.logical_frame, None);
        assert_eq!(result.raw_pixel_width, None);
        assert_eq!(result.total_frames_in_cycle, 0);
        assert_eq!(result.elapsed_frames, 0);

        let result = analyze_width(&mut engine, 0, false);
        assert_eq!(result.logical_frame, Some(0));
        assert_eq!(result.total_frames_in_cycle, 30);
        assert_eq!(result.elapsed_frames, 0);

        let result = analyze_width(&mut engine, 5, false);
        assert_eq!(result.logical_frame, Some(5));
        assert_eq!(result.elapsed_frames, 5);
    }

    #[test]
    fn first_detected_phase_after_battle_begin_counts_entering_frames() {
        let mut engine = engine_with_profiles(&[30]);

        let result = analyze_width_with_state(&mut engine, 0, false, BattleState::BattleBegin);
        assert_eq!(result.elapsed_frames, 0);

        let result = analyze_width_with_state(&mut engine, 3, false, BattleState::OneXRunning);
        assert_eq!(result.logical_frame, Some(3));
        assert_eq!(result.elapsed_frames, 3);

        let result = analyze_width_with_state(&mut engine, 6, false, BattleState::OneXRunning);
        assert_eq!(result.logical_frame, Some(6));
        assert_eq!(result.elapsed_frames, 6);
    }

    #[test]
    fn battle_begin_reset_happens_once_so_undo_can_survive_title_screen() {
        let mut engine = engine_with_profiles(&[30]);

        analyze_width(&mut engine, 0, false);
        let result = analyze_width(&mut engine, 20, false);
        assert_eq!(result.elapsed_frames, 20);

        let result = analyze_width_with_state(&mut engine, 0, false, BattleState::BattleBegin);
        assert_eq!(result.elapsed_frames, 0);

        engine.adjust_timer(20);

        let result = analyze_width_with_state(&mut engine, 0, false, BattleState::BattleBegin);
        assert_eq!(result.battle_state, BattleState::BattleBegin);
        assert_eq!(result.elapsed_frames, 20);

        let result = analyze_width(&mut engine, 0, false);
        assert_eq!(result.elapsed_frames, 20);

        let result = analyze_width(&mut engine, 5, false);
        assert_eq!(result.elapsed_frames, 25);
    }

    #[test]
    fn settings_return_does_not_reanchor_on_non_natural_phase_rewind() {
        let mut engine = engine_with_profiles(&[30]);

        analyze_width(&mut engine, 0, false);
        let result = analyze_width(&mut engine, 6, false);
        assert_eq!(result.elapsed_frames, 6);

        let result = analyze_width_with_state(&mut engine, 0, false, BattleState::NotInBattle);
        assert_eq!(result.elapsed_frames, 6);

        let result = analyze_width_with_state(&mut engine, 0, false, BattleState::OneXRunning);
        assert_eq!(result.logical_frame, Some(6));
        assert_eq!(result.elapsed_frames, 6);

        let result = analyze_width_with_state(&mut engine, 14, false, BattleState::OneXRunning);
        assert_eq!(result.logical_frame, Some(14));
        assert_eq!(result.elapsed_frames, 14);
    }

    #[test]
    fn pause_or_settings_overlay_mid_battle_keeps_elapsed_time() {
        let mut engine = engine_with_profiles(&[30]);

        analyze_width(&mut engine, 0, false);
        let result = analyze_width(&mut engine, 20, false);
        assert_eq!(result.elapsed_frames, 20);

        // Opening settings mid-battle flickers a `BeforeOrAfterBattle` frame before
        // settling into the menu. Coming straight from battle, it is a transient
        // overlay: suppressed to NotInBattle and must not change elapsed time.
        let result =
            analyze_width_with_state(&mut engine, 0, false, BattleState::BeforeOrAfterBattle);
        assert_eq!(result.battle_state, BattleState::NotInBattle);
        assert_eq!(result.elapsed_frames, 20);

        let result = analyze_width_with_state(&mut engine, 0, false, BattleState::NotInBattle);
        assert_eq!(result.elapsed_frames, 20);

        // The same battle resumes; the timer carries on.
        let result = analyze_width(&mut engine, 25, false);
        assert_eq!(result.logical_frame, Some(25));
        assert_eq!(result.elapsed_frames, 25);
    }

    #[test]
    fn short_mid_battle_banner_does_not_poison_later_real_prebattle_banner() {
        let mut engine = engine_with_profiles(&[30]);

        analyze_width(&mut engine, 0, false);
        let result = analyze_width(&mut engine, 20, false);
        assert_eq!(result.elapsed_frames, 20);

        // A brief false `BeforeOrAfterBattle` flicker while leaving the battle is
        // suppressed and must not affect the next *real* pre-battle banner after
        // a long settlement/menu stretch.
        let result =
            analyze_width_with_state(&mut engine, 0, false, BattleState::BeforeOrAfterBattle);
        assert_eq!(result.battle_state, BattleState::NotInBattle);
        assert_eq!(result.elapsed_frames, 20);

        for _ in 0..PRE_BATTLE_BANNER_MIN_OUT_FRAMES {
            analyze_width_with_state(&mut engine, 0, false, BattleState::NotInBattle);
        }

        let result =
            analyze_width_with_state(&mut engine, 0, false, BattleState::BeforeOrAfterBattle);
        assert_eq!(result.battle_state, BattleState::BeforeOrAfterBattle);
        assert_eq!(result.elapsed_frames, 20);

        let result = analyze_width(&mut engine, 5, false);
        assert_eq!(result.elapsed_frames, 20);
    }

    #[test]
    fn exiting_battle_keeps_elapsed_time_until_next_battle() {
        let mut engine = engine_with_profiles(&[30]);

        analyze_width(&mut engine, 0, false);
        analyze_width(&mut engine, 20, false);

        let result =
            analyze_width_with_state(&mut engine, 0, false, BattleState::BeforeOrAfterBattle);
        assert_eq!(result.battle_state, BattleState::NotInBattle);
        assert_eq!(result.logical_frame, None);
        assert_eq!(result.elapsed_frames, 20);

        let result = analyze_width_with_state(&mut engine, 0, false, BattleState::NotInBattle);
        assert_eq!(result.logical_frame, None);
        assert_eq!(result.total_frames_in_cycle, 30);
        assert_eq!(result.elapsed_frames, 20);
    }

    #[test]
    fn point_two_x_deployment_keeps_elapsed_time_until_battle_resumes() {
        let mut engine = engine_with_profiles(&[30]);

        analyze_width(&mut engine, 0, false);
        let result = analyze_width(&mut engine, 20, false);
        assert_eq!(result.elapsed_frames, 20);

        let result = analyze_width_with_state(&mut engine, 20, false, BattleState::PointTwoXPaused);
        assert_eq!(result.battle_state, BattleState::PointTwoXPaused);
        assert_eq!(result.elapsed_frames, 20);

        let result = analyze_width_with_state(&mut engine, 0, false, BattleState::NotInBattle);
        assert_eq!(result.battle_state, BattleState::NotInBattle);
        assert_eq!(result.elapsed_frames, 20);

        let result =
            analyze_width_with_state(&mut engine, 0, false, BattleState::BeforeOrAfterBattle);
        assert_eq!(result.battle_state, BattleState::NotInBattle);
        assert_eq!(result.elapsed_frames, 20);

        let result = analyze_width_with_state(&mut engine, 20, false, BattleState::OneXPaused);
        assert_eq!(result.battle_state, BattleState::OneXPaused);
        assert_eq!(result.elapsed_frames, 20);
    }

    #[test]
    fn paused_battle_states_still_analyze_cost_bar() {
        let mut engine = engine_with_profiles(&[30]);

        analyze_width(&mut engine, 0, false);
        let result = analyze_width_with_state(&mut engine, 6, false, BattleState::OneXPaused);

        assert_eq!(result.logical_frame, Some(6));
        assert_eq!(result.total_frames_in_cycle, 30);
        assert_eq!(result.elapsed_frames, 6);
    }

    #[test]
    fn unreadable_bar_in_paused_battle_preserves_phase_anchor_across_settings() {
        let mut engine = engine_with_profiles(&[30]);

        // Battle runs and the bar advances to phase 0.233 (frame 7), i.e. the
        // user opens settings *before* the bar reaches half.
        analyze_width(&mut engine, 0, false);
        let result = analyze_width(&mut engine, 7, false);
        assert_eq!(result.logical_frame, Some(7));
        assert_eq!(result.elapsed_frames, 7);

        // The settings menu is up and the game is paused. For a couple of frames
        // the cost bar is still classified as an in-battle (paused) state but is
        // momentarily unreadable (the menu fade obscures it). The phase anchor
        // must survive this; clearing it is what used to corrupt the timer.
        let result = analyze_width_with_state(&mut engine, 40, false, BattleState::OneXPaused);
        assert_eq!(result.logical_frame, None);
        assert_eq!(result.elapsed_frames, 7);

        // Then the menu settles into a NotInBattle stretch.
        let result = analyze_width_with_state(&mut engine, 0, false, BattleState::NotInBattle);
        assert_eq!(result.elapsed_frames, 7);

        // On exit, the resume fade briefly reports a false `phase == 0` frame.
        // With the anchor preserved at 0.233 this rewind is rejected, so the
        // timer does not re-anchor to zero.
        let result = analyze_width_with_state(&mut engine, 0, false, BattleState::OneXRunning);
        assert_eq!(result.logical_frame, Some(7));
        assert_eq!(result.elapsed_frames, 7);

        // The real phase reappears (0.467). Only the genuine 0.233 -> 0.467
        // advance is counted (+7); the false zero must not inflate it to +14.
        let result = analyze_width_with_state(&mut engine, 14, false, BattleState::OneXRunning);
        assert_eq!(result.logical_frame, Some(14));
        assert_eq!(result.elapsed_frames, 14);
    }

    #[test]
    fn fp24_boundary_cycle_reports_extra_frame() {
        let mut engine = fp24_engine_with_profiles(&[30], 30);

        let result = advance_to_fp24_boundary_cycle(&mut engine);

        assert_eq!(result.logical_frame, Some(0));
        assert_eq!(result.total_frames_in_cycle, 31);
        assert_eq!(result.elapsed_frames, 300);
    }

    #[test]
    fn fp24_before_boundary_uses_normal_cycle_length() {
        let mut engine = fp24_engine_with_profiles(&[30], 30);

        let result = analyze_width(&mut engine, 0, false);
        assert_eq!(result.logical_frame, Some(0));
        assert_eq!(result.total_frames_in_cycle, 30);

        let result = analyze_width(&mut engine, 1, false);
        assert_eq!(result.logical_frame, Some(1));

        let result = analyze_width(&mut engine, 29, false);
        assert_eq!(result.logical_frame, Some(29));

        let result = analyze_width(&mut engine, 30, false);
        assert_eq!(result.logical_frame, Some(29));
        assert_eq!(result.total_frames_in_cycle, 30);
    }

    #[test]
    fn fp24_after_boundary_returns_to_normal_cycle_length() {
        let mut engine = fp24_engine_with_profiles(&[30], 30);
        advance_to_fp24_boundary_cycle(&mut engine);

        let result = analyze_width(&mut engine, 30, false);
        assert_eq!(result.logical_frame, Some(30));
        assert_eq!(result.total_frames_in_cycle, 31);
        assert_eq!(result.elapsed_frames, 330);

        let result = analyze_width(&mut engine, 1, false);
        assert_eq!(result.logical_frame, Some(0));
        assert_eq!(result.total_frames_in_cycle, 30);
        assert_eq!(result.elapsed_frames, 331);

        let result = analyze_width(&mut engine, 2, false);
        assert_eq!(result.logical_frame, Some(1));

        let result = analyze_width(&mut engine, 29, false);
        assert_eq!(result.logical_frame, Some(28));

        let result = analyze_width(&mut engine, 30, false);
        assert_eq!(result.logical_frame, Some(29));
    }

    #[test]
    fn fp24_boundary_cycle_entering_negative_cost_keeps_elapsed_frames_unscaled() {
        let mut engine = fp24_engine_with_profiles(&[30], 30);
        advance_to_fp24_boundary_cycle(&mut engine);

        let result = analyze_width(&mut engine, 15, true);

        assert_eq!(result.total_frames_in_cycle, 46);
        assert_eq!(result.logical_frame, Some(15));
    }

    fn engine_with_profiles(total_frames: &[i32]) -> Analyzer {
        let mut engine = Analyzer::new();
        engine.set_roi_value(TEST_ROI);
        engine.set_loaded_calibration(loaded_fp24_calibration(total_frames, 10_000));
        engine
    }

    fn fp24_engine_with_profiles(total_frames: &[i32], total_bar_width: i32) -> Analyzer {
        let mut engine = Analyzer::new();
        engine.set_roi_value(TEST_ROI);
        engine.set_loaded_calibration(loaded_fp24_calibration(total_frames, total_bar_width));
        engine
    }

    fn loaded_fp24_calibration(total_frames: &[i32], total_bar_width: i32) -> LoadedCalibration {
        let profile_maps = total_frames
            .iter()
            .map(|total_frames| {
                let pairs = (1..*total_frames)
                    .map(|width| (width, width))
                    .collect::<Vec<_>>();
                (*total_frames, pairs)
            })
            .collect::<Vec<_>>();
        let borrowed_maps = profile_maps
            .iter()
            .map(|(frames, pairs)| (*frames, pairs.as_slice()))
            .collect::<Vec<_>>();
        loaded_calibration_from_maps(total_bar_width, &borrowed_maps)
    }

    fn loaded_calibration_from_maps(
        total_bar_width: i32,
        profile_maps: &[(i32, &[(i32, i32)])],
    ) -> LoadedCalibration {
        let profiles = profile_maps
            .iter()
            .map(|(total_frames, _)| ProfileData {
                total_frames: *total_frames,
            })
            .collect::<Vec<_>>();
        let required = Fp24::required_from_frame_ratio(
            profile_maps
                .iter()
                .map(|(total_frames, _)| *total_frames)
                .sum::<i32>(),
            profile_maps.len(),
        );
        let tables = profile_maps
            .iter()
            .map(|(total_frames, pairs)| {
                let pixel_map = pairs
                    .iter()
                    .map(|(width, frame)| (width.to_string(), *frame))
                    .collect::<HashMap<_, _>>();
                CalibrationTable::from_pixel_map(&pixel_map, *total_frames)
            })
            .collect::<Vec<_>>();

        LoadedCalibration {
            data: CalibrationData {
                format_version: crate::analysis::calibration::CALIBRATION_FORMAT_VERSION,
                timing_model: TIMING_MODEL_FP24_ACCUMULATOR_V1.to_string(),
                required_fp: required.raw(),
                profiles,
            },
            tables,
            timing_model: CalibrationTimingModel::Fp24AccumulatorV1 { required },
            total_bar_width,
            bar_width_frac: total_bar_width as f64,
        }
    }

    fn advance_to_fp24_boundary_cycle(engine: &mut Analyzer) -> FrameResult {
        let mut result = analyze_width(engine, 0, false);
        for _ in 0..10 {
            analyze_width(engine, 29, false);
            result = analyze_width(engine, 0, false);
        }
        result
    }

    fn analyze_width(engine: &mut Analyzer, raw_width: i32, cost_is_negative: bool) -> FrameResult {
        analyze_width_with_state(
            engine,
            raw_width,
            cost_is_negative,
            BattleState::OneXRunning,
        )
    }

    fn analyze_width_at(
        engine: &mut Analyzer,
        raw_width: i32,
        cost_is_negative: bool,
        logical_frame: u64,
    ) -> FrameResult {
        let timestamp_ns = logical_frame.saturating_mul(1_000_000_000 / 30);
        let buffer = make_bgr_frame(raw_width, cost_is_negative);
        engine
            .analyze_frame_with_battle_state_at(
                &buffer,
                TEST_SCREEN_WIDTH,
                TEST_SCREEN_HEIGHT,
                PixelFormat::Bgr,
                BattleState::OneXRunning,
                Some(timestamp_ns),
            )
            .unwrap()
    }

    fn analyze_width_with_state(
        engine: &mut Analyzer,
        raw_width: i32,
        cost_is_negative: bool,
        battle_state: BattleState,
    ) -> FrameResult {
        let buffer = make_bgr_frame(raw_width, cost_is_negative);
        engine
            .analyze_frame_with_battle_state(
                &buffer,
                TEST_SCREEN_WIDTH,
                TEST_SCREEN_HEIGHT,
                PixelFormat::Bgr,
                battle_state,
            )
            .unwrap()
    }

    fn make_bgr_frame(raw_width: i32, cost_is_negative: bool) -> Vec<u8> {
        let mut buffer = vec![30u8; (TEST_SCREEN_WIDTH * TEST_SCREEN_HEIGHT * 3) as usize];
        let filled_width = raw_width.clamp(0, TEST_ROI.1 - TEST_ROI.0);
        for x in TEST_ROI.0..(TEST_ROI.0 + filled_width) {
            put_bgr_screen_pixel(
                &mut buffer,
                TEST_SCREEN_WIDTH,
                TEST_SCREEN_HEIGHT,
                x,
                TEST_ROI.2,
                [252, 252, 252],
            );
        }

        if cost_is_negative {
            for y in 514..517 {
                for x in 1210..1231 {
                    put_bgr_screen_pixel(
                        &mut buffer,
                        TEST_SCREEN_WIDTH,
                        TEST_SCREEN_HEIGHT,
                        x,
                        y,
                        [255, 255, 255],
                    );
                }
            }
        }

        buffer
    }

    fn put_bgr_screen_pixel(buf: &mut [u8], width: u32, height: u32, x: i32, y: i32, rgb: [u8; 3]) {
        let offset = ((height as i32 - 1 - y) as u32 * width * 3 + x as u32 * 3) as usize;
        buf[offset] = rgb[2];
        buf[offset + 1] = rgb[1];
        buf[offset + 2] = rgb[0];
    }
}
