use std::path::Path;

use crate::analysis::calibration::LoadedCalibration;
use crate::analysis::roi::{self, Roi};
use crate::analysis::scanner::{self, BattleState, PixelFormat};
use crate::analysis::synthesis::synthesized_width_for_phase;
use crate::capture::CapturedFrame;
use crate::fp24::{Fp24, Fp24CostTiming};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameResult {
    pub logical_frame: Option<i32>,
    pub total_frames_in_cycle: i32,
    pub raw_pixel_width: Option<i32>,
    pub elapsed_frames: i32,
    pub cost_is_negative: bool,
    pub battle_state: BattleState,
    /// Fixed-point timing diagnostics for the committed frame (None when the
    /// bar is unreadable / frozen this frame).
    pub timing_debug: Option<TimingDebug>,
}

/// Per-frame fp24 timing diagnostics, exported to the API and debug CSV so a
/// recording can be replayed and the accumulator behavior verified offline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimingDebug {
    pub required_fp: i64,
    pub speed_fp: i64,
    pub accumulator_fp: i64,
    /// Frames the accumulator advanced this capture (the committed resync step).
    pub advanced_frames: i32,
    pub frames_since_cycle_start: i32,
    pub frames_until_next_cost: i32,
    pub match_error_px: i32,
    /// Whether the negative-cost boundary sub-frame correction fired this frame.
    pub boundary_corrected: bool,
}

/// Layer 2 analyzer — holds calibration, ROI, and fp24 timing state. Does NOT
/// own a capture backend; that responsibility belongs to Layer 1
/// ([`crate::pipeline::CapturePipeline`]).
///
/// The timing core is a forward-simulating fp24 accumulator: each captured frame
/// steps it forward by the fewest logical frames whose *rendered* bar width best
/// matches the *observed* width, then advances the elapsed-frame counter by that
/// same step. The boundary "extra frame" and X.5 alternation fall out of the
/// fixed-point arithmetic; there is no pixel-map lookup or endpoint bookkeeping.
pub struct Analyzer {
    calibration: Option<LoadedCalibration>,
    roi: Option<Roi>,
    bar_width_frac: Option<f64>,
    ui_scaler: f64,
    fp24: Option<Fp24CostTiming>,
    /// Phase of the last *committed* (accepted) frame. Gates the anti-spurious
    /// wrap guard: a resync may cross a cost-recovery boundary only when the bar
    /// was already late in its cycle, so a momentary false width drop cannot be
    /// read as "advanced most of a cycle".
    last_accepted_phase: Option<f64>,
    last_cost_is_negative: bool,
    last_known_cycle_total_frames: i32,
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
    elapsed_frames: i32,
    last_reported_battle_state: Option<BattleState>,
}

/// Minimum consecutive out-of-battle frames before a `BeforeOrAfterBattle` frame
/// is trusted as a genuine pre-battle banner (rather than a brief mid-battle
/// overlay). Observed overlay flickers last only a handful of frames, while real
/// loading/settlement stretches run into the dozens-to-hundreds.
const PRE_BATTLE_BANNER_MIN_OUT_FRAMES: u32 = 30;

/// Maximum pixel error between the rendered and observed bar width before a
/// readable frame is treated as unreadable (occlusion / HUD contamination) and
/// frozen. Mirrors the old table-lookup `WIDTH_MATCH_TOLERANCE`.
const MATCH_TOLERANCE_PX: i32 = 5;

/// A resync may cross a cost-recovery boundary (wrap into the next cycle) only
/// when the previously accepted phase was at least this far into the cycle.
const WRAP_MIN_PREV_PHASE: f64 = 0.75;

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
            fp24: None,
            last_accepted_phase: None,
            last_cost_is_negative: false,
            last_known_cycle_total_frames: 0,
            out_of_battle_frames: 0,
            battle_begin_reset_armed: true,
            elapsed_frames: 0,
            last_reported_battle_state: None,
        }
    }

    pub fn load_calibration<P: AsRef<Path>>(&mut self, path: P) -> Result<(), String> {
        log::info!("loading calibration from '{}'", path.as_ref().display());
        let loaded = LoadedCalibration::from_file(path.as_ref())?;
        self.set_loaded_calibration(loaded);
        Ok(())
    }

    pub fn load_calibration_json(&mut self, json: &str) -> Result<(), String> {
        let loaded = LoadedCalibration::from_json(json)?;
        self.set_loaded_calibration(loaded);
        Ok(())
    }

    /// Unload the current calibration. After this, [`Self::analyze_captured_frame`]
    /// returns `Err("No calibration loaded")` until a new profile is loaded.
    pub fn clear_calibration(&mut self) {
        self.calibration = None;
        self.fp24 = None;
    }

    /// Whether a calibration profile is currently loaded.
    pub fn has_calibration(&self) -> bool {
        self.calibration.is_some()
    }

    pub fn analyze_captured_frame(
        &mut self,
        frame_data: &CapturedFrame,
    ) -> Result<FrameResult, String> {
        self.analyze_frame(
            &frame_data.data,
            frame_data.width,
            frame_data.height,
            frame_data.format,
        )
    }

    pub fn analyze_raw_buffer(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<FrameResult, String> {
        self.analyze_frame(buffer, width, height, format)
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
        self.elapsed_frames = 0;
        self.last_known_cycle_total_frames = 0;
        self.last_cost_is_negative = false;
        self.last_accepted_phase = None;
        if let Some(required) = self.required() {
            self.fp24 = Some(Fp24CostTiming::new(required));
        }
    }

    pub fn adjust_timer(&mut self, frames: i32) {
        // The user is correcting the displayed timer, not the bar phase; leave
        // the fp24 accumulator alone so pixel tracking continues uninterrupted.
        self.elapsed_frames = self.elapsed_frames.saturating_add(frames);
    }

    /// No-op retained for command-protocol compatibility. The fp24 model has a
    /// single `required` value (X.5 alternation and the boundary cycle are
    /// intrinsic), so there is no multi-profile index to select.
    pub fn set_profile_index(&mut self, _index: usize) {}

    fn required(&self) -> Option<Fp24> {
        self.calibration.as_ref().map(|cal| cal.required())
    }

    fn set_loaded_calibration(&mut self, loaded: LoadedCalibration) {
        let required = loaded.required();
        self.calibration = Some(loaded);
        self.fp24 = Some(Fp24CostTiming::new(required));
        self.last_accepted_phase = None;
        self.last_cost_is_negative = false;
        self.last_known_cycle_total_frames = 0;
        self.out_of_battle_frames = 0;
        self.battle_begin_reset_armed = true;
        self.last_reported_battle_state = None;
    }

    fn analyze_frame(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<FrameResult, String> {
        let battle_state = scanner::detect_battle_state_with_ui_scaler(
            buffer,
            width,
            height,
            format,
            self.ui_scaler,
        );
        self.analyze_frame_with_battle_state(buffer, width, height, format, battle_state)
    }

    fn analyze_frame_with_battle_state(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
        battle_state: BattleState,
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
        }

        if self.calibration.is_none() {
            return Err("No calibration loaded".to_string());
        }
        let roi = self.roi.ok_or_else(|| {
            "No ROI set - call connect() first or use set_roi()/set_roi_value()".to_string()
        })?;

        // Out-of-battle (loading / settlement / suppressed overlay): freeze the
        // timer and hide the frame, preserving all last-known values.
        if !battle_state.is_in_battle() {
            return Ok(self.frozen_result(battle_state, None, self.last_cost_is_negative));
        }

        let total_bar_width = roi.1 - roi.0;
        let bar_width_frac = self.bar_width_frac.unwrap_or(total_bar_width as f64);
        let required = self.required().expect("calibration present");
        let base = self
            .fp24
            .get_or_insert_with(|| Fp24CostTiming::new(required));
        let base = *base;

        let observed = scanner::get_raw_filled_pixel_width(buffer, width, height, format, roi);
        let neg =
            scanner::is_cost_negative_with_ui_scaler(buffer, width, height, format, self.ui_scaler);
        let speed = speed_for_cost_state(neg);

        let Some(observed) = observed else {
            // Bar momentarily unreadable while still in battle (deployment
            // slow-mo, menu fade). Freeze; the accumulator stays put and
            // re-locks when a clean reading returns.
            return Ok(self.frozen_result(battle_state, None, neg));
        };

        let allow_wrap = self
            .last_accepted_phase
            .is_some_and(|phase| phase > WRAP_MIN_PREV_PHASE);
        let is_transition = self.last_accepted_phase.is_some() && neg != self.last_cost_is_negative;

        let outcome = resync(
            base,
            speed,
            observed,
            total_bar_width,
            bar_width_frac,
            allow_wrap,
            is_transition,
        );

        // D5 reject gate: a readable but poorly matching frame is contaminated
        // (occlusion / HUD). Do not commit; freeze so it cannot skew the timer.
        if outcome.match_error_px > MATCH_TOLERANCE_PX {
            return Ok(self.frozen_result(battle_state, Some(observed), neg));
        }

        // Commit the resync.
        self.fp24 = Some(outcome.timing);
        self.elapsed_frames = self.elapsed_frames.saturating_add(outcome.advanced_frames);
        self.last_accepted_phase = Some(outcome.phase);
        self.last_cost_is_negative = neg;

        let frames_since_cycle_start = outcome.timing.frames_since_cycle_start();
        let frames_until_next_cost = outcome.timing.frames_until_next_cost(speed);
        let total_frames_in_cycle = frames_since_cycle_start.saturating_add(frames_until_next_cost);
        self.last_known_cycle_total_frames = total_frames_in_cycle;

        let timing_debug = TimingDebug {
            required_fp: required.raw(),
            speed_fp: speed.raw(),
            accumulator_fp: outcome.timing.accumulator().raw(),
            advanced_frames: outcome.advanced_frames,
            frames_since_cycle_start,
            frames_until_next_cost,
            match_error_px: outcome.match_error_px,
            boundary_corrected: outcome.boundary_corrected,
        };

        let result = FrameResult {
            logical_frame: Some(frames_since_cycle_start),
            total_frames_in_cycle,
            raw_pixel_width: Some(observed),
            elapsed_frames: self.elapsed_frames,
            cost_is_negative: neg,
            battle_state,
            timing_debug: Some(timing_debug),
        };
        log::trace!(
            "frame summary: battle_state={}, logical_frame={:?}, total={}, raw_width={:?}, elapsed={}, negative={}, advanced={}, err={}",
            result.battle_state.as_str(),
            result.logical_frame,
            result.total_frames_in_cycle,
            result.raw_pixel_width,
            result.elapsed_frames,
            result.cost_is_negative,
            outcome.advanced_frames,
            outcome.match_error_px,
        );
        Ok(result)
    }

    /// Build a frozen result: the timer and cycle length hold their last-known
    /// values and no logical frame is reported. `raw_pixel_width` carries the
    /// observed width when the bar was readable-but-rejected, `None` otherwise.
    fn frozen_result(
        &self,
        battle_state: BattleState,
        raw_pixel_width: Option<i32>,
        cost_is_negative: bool,
    ) -> FrameResult {
        FrameResult {
            logical_frame: None,
            total_frames_in_cycle: self.last_known_cycle_total_frames,
            raw_pixel_width,
            elapsed_frames: self.elapsed_frames,
            cost_is_negative,
            battle_state,
            timing_debug: None,
        }
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

fn speed_for_cost_state(cost_is_negative: bool) -> Fp24 {
    if cost_is_negative {
        Fp24::NEGATIVE_SPEED
    } else {
        Fp24::NORMAL_SPEED
    }
}

fn normalized_ui_scaler(ui_scaler: f64) -> f64 {
    if ui_scaler.is_finite() {
        ui_scaler.clamp(0.0, 1.0)
    } else {
        roi::DEFAULT_UI_SCALER
    }
}

#[derive(Clone, Copy, Debug)]
struct ResyncOutcome {
    timing: Fp24CostTiming,
    advanced_frames: i32,
    phase: f64,
    match_error_px: i32,
    boundary_corrected: bool,
}

/// Render the bar width for `timing`'s current phase and return `|width - observed|`.
fn phase_pixel_error(
    timing: Fp24CostTiming,
    observed: i32,
    total_bar_width: i32,
    bar_width_frac: f64,
) -> (f64, i32) {
    let phase = timing.phase();
    let width = synthesized_width_for_phase(total_bar_width, bar_width_frac, phase);
    (phase, (width - observed).abs())
}

/// Find the fewest forward logical frames that make the rendered width best match
/// `observed`, honoring the anti-spurious wrap guard, then optionally apply the
/// negative-cost boundary sub-frame correction.
fn resync(
    base: Fp24CostTiming,
    speed: Fp24,
    observed: i32,
    total_bar_width: i32,
    bar_width_frac: f64,
    allow_wrap: bool,
    is_transition: bool,
) -> ResyncOutcome {
    // One full cycle at the current speed bounds the search ("at most one cycle").
    let period = Fp24CostTiming::new(base.required())
        .frames_until_next_cost(speed)
        .max(1);
    let max_advance = period + 1;

    // K = 0 is always a legal candidate (no boundary crossing).
    let (phase0, err0) = phase_pixel_error(base, observed, total_bar_width, bar_width_frac);
    let mut best = ResyncOutcome {
        timing: base,
        advanced_frames: 0,
        phase: phase0,
        match_error_px: err0,
        boundary_corrected: false,
    };

    let mut timing = base;
    for k in 1..=max_advance {
        timing.advance_one_frame(speed);
        let crossed_boundary = timing.cycle_index() > base.cycle_index();
        if crossed_boundary && !allow_wrap {
            // Not permitted to cross a recovery boundary from an early phase;
            // every larger K also crosses, so stop searching.
            break;
        }
        let (phase, err) = phase_pixel_error(timing, observed, total_bar_width, bar_width_frac);
        if err < best.match_error_px {
            best = ResyncOutcome {
                timing,
                advanced_frames: k,
                phase,
                match_error_px: err,
                boundary_corrected: false,
            };
        }
    }

    if is_transition {
        best = apply_boundary_subframe_correction(best, observed, total_bar_width, bar_width_frac);
    }

    best
}

/// Negative-cost boundary sub-frame correction. On the frame where the cost
/// state flips, the transition frame's speed may have been mis-assigned by one
/// frame, leaving the accumulator off by ~one half-speed increment (half a
/// normal frame). Nudge the accumulator ±that increment if it reduces the pixel
/// error — a pure phase re-alignment that does not change the elapsed count.
fn apply_boundary_subframe_correction(
    best: ResyncOutcome,
    observed: i32,
    total_bar_width: i32,
    bar_width_frac: f64,
) -> ResyncOutcome {
    let half_increment = Fp24::NEGATIVE_SPEED.raw();
    let mut corrected = best;
    for delta in [half_increment, -half_increment] {
        let mut nudged = best.timing;
        nudged.nudge_accumulator_raw(delta);
        let (phase, err) = phase_pixel_error(nudged, observed, total_bar_width, bar_width_frac);
        if err < corrected.match_error_px {
            corrected = ResyncOutcome {
                timing: nudged,
                advanced_frames: best.advanced_frames,
                phase,
                match_error_px: err,
                boundary_corrected: true,
            };
        }
    }
    corrected
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SCREEN_WIDTH: u32 = 1280;
    const TEST_SCREEN_HEIGHT: u32 = 720;
    // A 180px-wide ROI gives a real (non-synthetic) geometry with ~6px/frame at
    // N=30, matching the validated closed-form used by the runtime renderer.
    const TEST_ROI: Roi = (200, 380, 400);
    const TEST_BAR_WIDTH: i32 = 180;

    fn engine_with_required(required: f64) -> Analyzer {
        let mut engine = Analyzer::new();
        engine.set_roi_value(TEST_ROI);
        engine
            .load_calibration_json(&format!(
                r#"{{ "format_version": 4, "required": {required} }}"#
            ))
            .unwrap();
        engine
    }

    /// The width the game renders at cycle-frame `frame` for `n_eff` frames/cost.
    fn width_at_frame(n_eff: f64, frame: i32) -> i32 {
        crate::analysis::synthesis::synthesized_width(
            TEST_BAR_WIDTH,
            TEST_BAR_WIDTH as f64,
            n_eff,
            frame,
        )
    }

    fn analyze_frame_at(engine: &mut Analyzer, n_eff: f64, frame: i32, neg: bool) -> FrameResult {
        analyze_observed(
            engine,
            width_at_frame(n_eff, frame),
            neg,
            BattleState::OneXRunning,
        )
    }

    fn analyze_observed(
        engine: &mut Analyzer,
        observed: i32,
        neg: bool,
        state: BattleState,
    ) -> FrameResult {
        let buffer = make_bgr_frame(observed, neg);
        engine
            .analyze_frame_with_battle_state(
                &buffer,
                TEST_SCREEN_WIDTH,
                TEST_SCREEN_HEIGHT,
                PixelFormat::Bgr,
                state,
            )
            .unwrap()
    }

    #[test]
    fn normal_cycle_tracks_frames_and_elapsed() {
        let mut engine = engine_with_required(1.0);

        let r = analyze_frame_at(&mut engine, 30.0, 0, false);
        assert_eq!(r.logical_frame, Some(0));
        assert_eq!(r.total_frames_in_cycle, 30);
        assert_eq!(r.elapsed_frames, 0);

        let r = analyze_frame_at(&mut engine, 30.0, 15, false);
        assert_eq!(r.logical_frame, Some(15));
        assert_eq!(r.elapsed_frames, 15);

        let r = analyze_frame_at(&mut engine, 30.0, 29, false);
        assert_eq!(r.logical_frame, Some(29));
        assert_eq!(r.elapsed_frames, 29);

        // Wrap into the next cycle from a late phase.
        let r = analyze_frame_at(&mut engine, 30.0, 0, false);
        assert_eq!(r.logical_frame, Some(0));
        assert_eq!(r.total_frames_in_cycle, 30);
        assert_eq!(r.elapsed_frames, 30);
    }

    #[test]
    fn first_frame_counts_entering_frames() {
        let mut engine = engine_with_required(1.0);
        let r = analyze_frame_at(&mut engine, 30.0, 8, false);
        assert_eq!(r.logical_frame, Some(8));
        assert_eq!(r.elapsed_frames, 8);
    }

    #[test]
    fn battle_begin_resets_elapsed() {
        let mut engine = engine_with_required(1.0);
        analyze_frame_at(&mut engine, 30.0, 0, false);
        let r = analyze_frame_at(&mut engine, 30.0, 20, false);
        assert_eq!(r.elapsed_frames, 20);

        let r = analyze_observed(&mut engine, 0, false, BattleState::BattleBegin);
        assert_eq!(r.logical_frame, None);
        assert_eq!(r.elapsed_frames, 0);

        let r = analyze_frame_at(&mut engine, 30.0, 5, false);
        assert_eq!(r.logical_frame, Some(5));
        assert_eq!(r.elapsed_frames, 5);
    }

    #[test]
    fn not_in_battle_freezes_and_hides_frame() {
        let mut engine = engine_with_required(1.0);
        analyze_frame_at(&mut engine, 30.0, 0, false);
        let r = analyze_frame_at(&mut engine, 30.0, 8, false);
        assert_eq!(r.elapsed_frames, 8);

        let r = analyze_observed(&mut engine, 0, false, BattleState::NotInBattle);
        assert_eq!(r.logical_frame, None);
        assert_eq!(r.raw_pixel_width, None);
        assert_eq!(r.elapsed_frames, 8);

        let r = analyze_frame_at(&mut engine, 30.0, 8, false);
        assert_eq!(r.logical_frame, Some(8));
        assert_eq!(r.elapsed_frames, 8);
    }

    #[test]
    fn spurious_zero_mid_cycle_does_not_jump_elapsed() {
        // D2 guard: at mid-cycle (phase ~0.5) a false 0px read must not be
        // resolved as "advanced most of a cycle" (+~15). The wrap solution is
        // forbidden from an early phase, so the frame is rejected and frozen.
        let mut engine = engine_with_required(1.0);
        analyze_frame_at(&mut engine, 30.0, 0, false);
        let r = analyze_frame_at(&mut engine, 30.0, 15, false);
        assert_eq!(r.elapsed_frames, 15);

        let r = analyze_observed(&mut engine, 0, false, BattleState::OneXRunning);
        assert_eq!(r.logical_frame, None, "spurious 0 must be rejected");
        assert_eq!(r.elapsed_frames, 15, "elapsed must not jump");

        // The real phase reappears; tracking resumes without inflation.
        let r = analyze_frame_at(&mut engine, 30.0, 16, false);
        assert_eq!(r.logical_frame, Some(16));
        assert_eq!(r.elapsed_frames, 16);
    }

    #[test]
    fn late_phase_wrap_is_allowed() {
        let mut engine = engine_with_required(1.0);
        analyze_frame_at(&mut engine, 30.0, 0, false);
        let r = analyze_frame_at(&mut engine, 30.0, 28, false);
        assert_eq!(r.elapsed_frames, 28);

        // From phase 0.93 a drop to empty is a genuine recovery wrap.
        let r = analyze_frame_at(&mut engine, 30.0, 0, false);
        assert_eq!(r.logical_frame, Some(0));
        assert_eq!(r.elapsed_frames, 30);
    }

    #[test]
    fn negative_cost_runs_at_half_speed() {
        let mut engine = engine_with_required(1.0);
        // Enter negative cost near the start; the cycle now spans ~60 frames.
        let r = analyze_frame_at(&mut engine, 60.0, 0, true);
        assert!(r.cost_is_negative);
        assert!(
            (r.total_frames_in_cycle - 60).abs() <= 1,
            "negative cycle ~60, got {}",
            r.total_frames_in_cycle
        );

        let r = analyze_frame_at(&mut engine, 60.0, 20, true);
        assert_eq!(r.logical_frame, Some(20));
        assert!((r.total_frames_in_cycle - 60).abs() <= 1);
    }

    #[test]
    fn readable_but_unmatchable_frame_is_frozen() {
        let mut engine = engine_with_required(1.0);
        analyze_frame_at(&mut engine, 30.0, 0, false);
        let r = analyze_frame_at(&mut engine, 30.0, 10, false);
        assert_eq!(r.elapsed_frames, 10);

        // A width that no reachable phase renders within tolerance (a small
        // early-phase reading, unreachable forward without crossing a boundary)
        // is rejected; the observed width is still surfaced, the timer holds.
        let r = analyze_observed(&mut engine, 3, false, BattleState::OneXRunning);
        assert_eq!(r.logical_frame, None);
        assert_eq!(r.raw_pixel_width, Some(3));
        assert_eq!(r.elapsed_frames, 10);
    }

    #[test]
    fn negative_transition_subframe_correction_realigns_phase() {
        // On a cost-state transition, a ~half-frame residual (the accumulator
        // sitting one half-speed increment ahead of the true phase, from a
        // mis-timed transition frame) is nudged back to a clean match without
        // changing the elapsed count.
        let required = Fp24::required_from_ratio(1.0);
        let half_increment = Fp24::NEGATIVE_SPEED.raw();
        let total_bar_width = TEST_BAR_WIDTH;
        let bar_width_frac = TEST_BAR_WIDTH as f64;

        let true_phase = 0.5;
        let observed = synthesized_width_for_phase(total_bar_width, bar_width_frac, true_phase);

        // Accumulator sits half a normal frame ahead of the true phase.
        let mut timing = Fp24CostTiming::new(required);
        let ahead_raw = (true_phase * required.raw() as f64) as i64 + half_increment;
        timing.nudge_accumulator_raw(ahead_raw);

        let (phase, err) = phase_pixel_error(timing, observed, total_bar_width, bar_width_frac);
        assert!(err >= 2, "expected a ~half-frame residual, got {err}px");
        let best = ResyncOutcome {
            timing,
            advanced_frames: 4,
            phase,
            match_error_px: err,
            boundary_corrected: false,
        };

        let corrected =
            apply_boundary_subframe_correction(best, observed, total_bar_width, bar_width_frac);
        assert!(
            corrected.boundary_corrected,
            "sub-frame correction should fire"
        );
        assert!(
            corrected.match_error_px <= 1,
            "phase realigned to ~0 error, got {}px",
            corrected.match_error_px
        );
        // The correction is a pure phase re-alignment; the elapsed step is unchanged.
        assert_eq!(corrected.advanced_frames, best.advanced_frames);
    }

    #[test]
    fn paused_and_deploying_states_still_track() {
        let mut engine = engine_with_required(1.0);
        analyze_frame_at(&mut engine, 30.0, 0, false);
        let r = analyze_observed(
            &mut engine,
            width_at_frame(30.0, 6),
            false,
            BattleState::OneXPaused,
        );
        assert_eq!(r.logical_frame, Some(6));
        assert_eq!(r.elapsed_frames, 6);

        let r = analyze_observed(
            &mut engine,
            width_at_frame(30.0, 9),
            false,
            BattleState::DeployingOperator,
        );
        assert_eq!(r.logical_frame, Some(9));
        assert_eq!(r.elapsed_frames, 9);
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
