use std::path::Path;

use crate::analysis::calibration::LoadedCalibration;
use crate::analysis::roi::{self, Roi};
use crate::analysis::scanner::{self, BattleState, PixelFormat};
use crate::capture::{create_backend, CaptureBackend, CaptureConfig, CapturedFrame};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameResult {
    pub logical_frame: Option<i32>,
    pub total_frames_in_cycle: i32,
    pub raw_pixel_width: Option<i32>,
    pub elapsed_frames: i32,
    pub cost_is_negative: bool,
    pub battle_state: BattleState,
}

pub struct RulerEngine {
    backend: Option<Box<dyn CaptureBackend>>,
    calibration: Option<LoadedCalibration>,
    roi: Option<Roi>,
    current_profile_index: usize,
    cycle_counter: usize,
    elapsed_frames: f64,
    previous_phase: Option<PhaseSample>,
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
}

#[derive(Clone, Copy, Debug)]
struct PhaseSample {
    phase: f64,
    total_frames: i32,
    cost_is_negative: bool,
}

impl PhaseSample {
    fn effective_total_frames(self) -> i32 {
        effective_total_frames(self.total_frames, self.cost_is_negative)
    }
}

#[derive(Clone, Copy, Debug)]
struct FrameLookup {
    logical_frame: i32,
    phase: f64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EngineStatus {
    pub connected: bool,
    pub has_calibration: bool,
    pub roi_ready: bool,
}

const NEGATIVE_COST_INTERVAL_MULTIPLIER: i32 = 2;

/// Minimum consecutive out-of-battle frames before a `BeforeOrAfterBattle` frame
/// is trusted as a genuine pre-battle banner (rather than a brief mid-battle
/// overlay). Observed overlay flickers last only a handful of frames, while real
/// loading/settlement stretches run into the dozens-to-hundreds.
const PRE_BATTLE_BANNER_MIN_OUT_FRAMES: u32 = 30;

impl Default for RulerEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl RulerEngine {
    pub fn new() -> Self {
        Self {
            backend: None,
            calibration: None,
            roi: None,
            current_profile_index: 0,
            cycle_counter: 0,
            elapsed_frames: 0.0,
            previous_phase: None,
            last_known_total_frames: 0,
            last_known_cycle_total_frames: 0,
            last_known_cost_is_negative: false,
            out_of_battle_frames: 0,
            battle_begin_reset_armed: true,
        }
    }

    pub fn connect(&mut self, config: CaptureConfig) -> Result<(u32, u32), String> {
        let mut backend = create_backend(config)?;
        backend.connect()?;

        let dims = backend.dimensions();
        self.roi = Some(roi::find_cost_bar_roi(dims.0 as i32, dims.1 as i32));
        self.backend = Some(backend);

        Ok(dims)
    }

    pub fn load_calibration<P: AsRef<Path>>(&mut self, path: P) -> Result<(), String> {
        let loaded = LoadedCalibration::from_file(path.as_ref())?;
        self.set_loaded_calibration(loaded);
        Ok(())
    }

    pub fn load_calibration_json(&mut self, json: &str) -> Result<(), String> {
        let loaded = LoadedCalibration::from_json(json)?;
        self.set_loaded_calibration(loaded);
        Ok(())
    }

    pub fn capture_and_analyze(&mut self) -> Result<FrameResult, String> {
        let frame_data = self.capture_frame()?;
        self.analyze_captured_frame(&frame_data)
    }

    pub fn capture_frame(&mut self) -> Result<CapturedFrame, String> {
        let mut backend = self
            .backend
            .take()
            .ok_or_else(|| "Not connected".to_string())?;

        let frame_data: CapturedFrame = backend.capture_frame()?;
        self.backend = Some(backend);

        Ok(frame_data)
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
        self.roi = Some(roi::find_cost_bar_roi(screen_width, screen_height));
    }

    pub fn set_roi_value(&mut self, roi: Roi) {
        self.roi = Some(roi);
    }

    pub fn roi(&self) -> Option<Roi> {
        self.roi
    }

    pub fn status(&self) -> EngineStatus {
        EngineStatus {
            connected: self.backend.is_some(),
            has_calibration: self.calibration.is_some(),
            roi_ready: self.roi.is_some(),
        }
    }

    pub fn reset_timer(&mut self) {
        self.elapsed_frames = 0.0;
        self.cycle_counter = 0;
        self.last_known_total_frames = 0;
        self.last_known_cycle_total_frames = 0;
        self.last_known_cost_is_negative = false;
        self.previous_phase = None;
    }

    pub fn adjust_timer(&mut self, frames: i32) {
        self.elapsed_frames += frames as f64;
        self.last_known_total_frames = rounded_frame_count(self.elapsed_frames);
    }

    pub fn set_profile_index(&mut self, index: usize) {
        if let Some(cal) = &self.calibration {
            if index < cal.tables.len() {
                self.cycle_counter = 0;
                self.previous_phase = None;
                self.current_profile_index = index;
            }
        }
    }

    pub fn disconnect(&mut self) {
        if let Some(mut backend) = self.backend.take() {
            backend.disconnect();
        }
    }

    fn set_loaded_calibration(&mut self, loaded: LoadedCalibration) {
        self.calibration = Some(loaded);
        self.current_profile_index = 0;
        self.cycle_counter = 0;
        self.previous_phase = None;
        self.last_known_cycle_total_frames = 0;
        self.last_known_cost_is_negative = false;
        self.last_known_total_frames = rounded_frame_count(self.elapsed_frames);
        self.out_of_battle_frames = 0;
        self.battle_begin_reset_armed = true;
    }

    fn analyze_frame(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<FrameResult, String> {
        let battle_state = scanner::detect_battle_state(buffer, width, height, format);
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

        if !battle_state.is_in_battle() {
            return Ok(FrameResult {
                logical_frame: None,
                total_frames_in_cycle: self.last_known_cycle_total_frames,
                raw_pixel_width: None,
                elapsed_frames: self.last_known_total_frames,
                cost_is_negative: self.last_known_cost_is_negative,
                battle_state,
            });
        }

        let calibration = self
            .calibration
            .as_ref()
            .ok_or_else(|| "No calibration loaded".to_string())?;

        let pixel_width = scanner::get_raw_filled_pixel_width(buffer, width, height, format, roi);
        let cost_is_negative = scanner::is_cost_negative(buffer, width, height, format);

        let num_profiles = calibration.tables.len();
        let base_profile = if num_profiles == 0 {
            0
        } else {
            self.current_profile_index.min(num_profiles - 1)
        };

        let (logical_frame, total_frames_in_cycle) = if num_profiles > 0 {
            let mut profile_idx = (base_profile + self.cycle_counter) % num_profiles;
            let mut table = &calibration.tables[profile_idx];
            let mut frame_lookup =
                pixel_width.and_then(|pw| lookup_bar_frame(table, pw, cost_is_negative));

            if let (Some(previous), Some(current), Some(pixel_width)) =
                (self.previous_phase, frame_lookup, pixel_width)
            {
                if is_natural_cycle_wrap(previous, current.phase) {
                    self.cycle_counter += 1;
                    profile_idx = (base_profile + self.cycle_counter) % num_profiles;
                    table = &calibration.tables[profile_idx];
                    frame_lookup =
                        lookup_bar_frame(table, pixel_width, cost_is_negative).or(frame_lookup);
                }
            }

            let total_frames = table.total_frames;
            let effective_total_frames = effective_total_frames(total_frames, cost_is_negative);
            let current_phase = frame_lookup.map(|lookup| PhaseSample {
                phase: lookup.phase,
                total_frames,
                cost_is_negative,
            });

            if let Some(current_phase) = current_phase {
                if let Some(previous_phase) = self.previous_phase {
                    let phase_delta = phase_delta(previous_phase, current_phase);
                    self.elapsed_frames +=
                        phase_delta * previous_phase.effective_total_frames() as f64;
                    self.last_known_total_frames = rounded_frame_count(self.elapsed_frames);
                }
                self.previous_phase = Some(current_phase);
            } else {
                self.previous_phase = None;
            }

            (
                frame_lookup.map(|lookup| lookup.logical_frame),
                effective_total_frames,
            )
        } else {
            self.previous_phase = None;
            (None, 0)
        };

        self.last_known_cycle_total_frames = total_frames_in_cycle;
        self.last_known_cost_is_negative = cost_is_negative;

        Ok(FrameResult {
            logical_frame,
            total_frames_in_cycle,
            raw_pixel_width: pixel_width,
            elapsed_frames: self.last_known_total_frames,
            cost_is_negative,
            battle_state,
        })
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

fn effective_total_frames(total_frames: i32, cost_is_negative: bool) -> i32 {
    if cost_is_negative {
        total_frames.saturating_mul(NEGATIVE_COST_INTERVAL_MULTIPLIER)
    } else {
        total_frames
    }
}

fn lookup_bar_frame(
    table: &crate::analysis::mapping::CalibrationTable,
    pixel_width: i32,
    cost_is_negative: bool,
) -> Option<FrameLookup> {
    let total_frames = table.total_frames;
    if total_frames <= 0 {
        return None;
    }

    if cost_is_negative {
        let phase = table.lookup_phase(pixel_width)?;
        let logical_frame = frame_from_phase(phase, effective_total_frames(total_frames, true));
        Some(FrameLookup {
            logical_frame,
            phase,
        })
    } else {
        let logical_frame = table.lookup(pixel_width)?;
        Some(FrameLookup {
            logical_frame,
            phase: logical_frame as f64 / total_frames as f64,
        })
    }
}

fn frame_from_phase(phase: f64, total_frames: i32) -> i32 {
    if total_frames <= 0 {
        return 0;
    }

    let frame = (phase.clamp(0.0, 1.0) * total_frames as f64).round() as i32;
    frame.clamp(0, total_frames - 1)
}

fn is_natural_cycle_wrap(previous: PhaseSample, current_phase: f64) -> bool {
    previous.phase > 0.75 && current_phase < 0.25
}

fn phase_delta(previous: PhaseSample, current: PhaseSample) -> f64 {
    let mut delta = current.phase - previous.phase;
    if delta < -0.5 {
        delta += 1.0;
    }
    delta.max(0.0)
}

fn rounded_frame_count(frames: f64) -> i32 {
    frames.round() as i32
}

impl Drop for RulerEngine {
    fn drop(&mut self) {
        self.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SCREEN_WIDTH: u32 = 1280;
    const TEST_SCREEN_HEIGHT: u32 = 720;
    const TEST_ROI: Roi = (100, 140, 100);

    #[test]
    fn analyze_raw_buffer_tracks_frames() {
        let mut engine = RulerEngine::new();
        engine
            .load_calibration_json(
                r#"{
                    "profiles": [{
                        "total_frames": 30,
                        "pixel_map": {"0": 0, "5": 1, "10": 2}
                    }]
                }"#,
            )
            .unwrap();
        engine.set_roi_value((0, 20, 0));

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
        assert_eq!(result.logical_frame, Some(0));
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

        assert_eq!(result.logical_frame, Some(30));
        assert_eq!(result.total_frames_in_cycle, 60);
        assert_eq!(result.elapsed_frames, 15);
        assert!(result.cost_is_negative);
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
        assert_eq!(result.elapsed_frames, 60);
        assert!(!result.cost_is_negative);
    }

    #[test]
    fn non_natural_negative_cost_exit_at_same_phase_does_not_jump_elapsed_time() {
        let mut engine = engine_with_profiles(&[30]);

        analyze_width(&mut engine, 0, false);
        analyze_width(&mut engine, 15, true);
        let result = analyze_width(&mut engine, 15, false);

        assert_eq!(result.logical_frame, Some(15));
        assert_eq!(result.total_frames_in_cycle, 30);
        assert_eq!(result.elapsed_frames, 15);
        assert!(!result.cost_is_negative);
    }

    #[test]
    fn negative_cost_interpolates_widths_missing_from_positive_profile() {
        let mut engine = RulerEngine::new();
        engine
            .load_calibration_json(
                r#"{
                    "profiles": [{
                        "total_frames": 8,
                        "pixel_map": {"0": 0, "3": 2, "5": 4, "7": 7}
                    }]
                }"#,
            )
            .unwrap();
        engine.set_roi_value(TEST_ROI);

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
    fn alternating_profiles_continue_and_negative_cost_doubles_current_effective_cycle() {
        let mut engine = engine_with_profiles(&[38, 37]);

        analyze_width(&mut engine, 0, false);
        analyze_width(&mut engine, 37, false);
        let result = analyze_width(&mut engine, 0, false);
        assert_eq!(result.logical_frame, Some(0));
        assert_eq!(result.total_frames_in_cycle, 37);
        assert_eq!(result.elapsed_frames, 38);

        let result = analyze_width(&mut engine, 10, true);
        assert_eq!(result.logical_frame, Some(20));
        assert_eq!(result.total_frames_in_cycle, 74);
        assert_eq!(result.elapsed_frames, 48);

        let result = analyze_width(&mut engine, 11, true);
        assert_eq!(result.logical_frame, Some(22));
        assert_eq!(result.total_frames_in_cycle, 74);
        assert_eq!(result.elapsed_frames, 50);
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

    fn engine_with_profiles(total_frames: &[i32]) -> RulerEngine {
        let mut engine = RulerEngine::new();
        engine
            .load_calibration_json(&calibration_json(total_frames))
            .unwrap();
        engine.set_roi_value(TEST_ROI);
        engine
    }

    fn calibration_json(total_frames: &[i32]) -> String {
        let profiles = total_frames
            .iter()
            .map(|total_frames| {
                let pixel_map = (0..*total_frames)
                    .map(|frame| format!(r#""{frame}": {frame}"#))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(r#"{{"total_frames": {total_frames}, "pixel_map": {{{pixel_map}}}}}"#)
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(r#"{{"profiles": [{profiles}]}}"#)
    }

    fn analyze_width(
        engine: &mut RulerEngine,
        raw_width: i32,
        cost_is_negative: bool,
    ) -> FrameResult {
        analyze_width_with_state(
            engine,
            raw_width,
            cost_is_negative,
            BattleState::OneXRunning,
        )
    }

    fn analyze_width_with_state(
        engine: &mut RulerEngine,
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
