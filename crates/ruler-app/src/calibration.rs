//! Calibration collector — a temporary Layer 1 InOrder consumer that
//! collects raw pixel-width samples for calibration inference.
//!
//! When the user initiates calibration, the worker connects a new InOrder
//! consumer to the pipeline and runs [`collect_calibration_samples`] on it.
//! The function blocks until two full cost-bar cycles have been collected
//! (or the user cancels). During this time, the L2 analyzer consumer
//! continues to run but its results are ignored — the UI shows the
//! calibration progress instead.

use ruler_core::{
    analysis::{
        calibration::infer_calibration_from_samples_with_ui_scaler_and_total_bar_width, scanner,
    },
    pipeline::ConsumerPipe,
};

use crate::worker::SharedAppState;

const CALIBRATION_CYCLES: usize = 2;

/// Sentinel error string used to distinguish a user-initiated cancellation
/// from a real failure inside [`collect_calibration_samples`].
pub const CALIBRATION_CANCELLED: &str = "calibration_cancelled";

/// Connect to the pipeline as an InOrder consumer and collect calibration
/// samples. Blocks until `CALIBRATION_CYCLES` full cycles have been collected
/// or the user cancels (via `state.take_cancel_calibration()`).
///
/// `pipe` must be a freshly-connected InOrder consumer. The function
/// consumes it (it is dropped when the function returns).
///
/// Returns the collected cycle samples plus screen dimensions and total bar
/// width — the inputs needed for
/// [`infer_calibration_from_samples_with_ui_scaler_and_total_bar_width`].
pub fn collect_calibration_samples(
    mut pipe: ConsumerPipe,
    state: &SharedAppState,
    roi: ruler_core::analysis::roi::Roi,
    ui_scaler: f64,
) -> Result<(Vec<Vec<i32>>, u32, u32, i32), String> {
    let total_bar_width = roi.1 - roi.0;
    if total_bar_width <= 0 {
        return Err("capture ROI has invalid width".to_string());
    }

    let first_frame = pipe
        .recv_frame()
        .map_err(|e| format!("calibration: first frame: {e}"))?;
    let screen_width = first_frame.width;
    let screen_height = first_frame.height;

    log::info!(
        "calibration: start collecting — roi=(x1={}, x2={}, y_mid={}) total_bar_width={} \
         frame={}x{} format={:?} ui_scaler={:.3}",
        roi.0,
        roi.1,
        roi.2,
        total_bar_width,
        screen_width,
        screen_height,
        first_frame.format,
        ui_scaler,
    );

    let mut cycle_samples: Vec<Vec<i32>> = Vec::new();
    let mut current_cycle_data: Vec<i32> = Vec::new();
    let mut previous_cost_state_raw: Option<i32> = None;
    let mut is_collecting_cycle = false;
    let mut progress = CalibrationProgress::new(total_bar_width);
    let mut frames_seen: u64 = 0;
    let mut some_count: u64 = 0;
    let mut none_streak: u64 = 0;
    let mut last_progress: f32 = 0.0;

    let mut frame = first_frame;

    while cycle_samples.len() < CALIBRATION_CYCLES {
        // Poll the cancel flag every frame so a right-click "cancel" aborts
        // the loop promptly instead of waiting for both cycles to complete.
        if state.take_cancel_calibration() {
            return Err(CALIBRATION_CANCELLED.to_string());
        }

        let current_cost_state_raw = scanner::get_raw_filled_pixel_width(
            &frame.data,
            frame.width,
            frame.height,
            frame.format,
            roi,
        );

        frames_seen += 1;

        if let Some(current) = current_cost_state_raw {
            some_count += 1;
            none_streak = 0;

            if let Some(previous) = previous_cost_state_raw {
                if (previous as f64) > total_bar_width as f64 * 0.9
                    && (current as f64) < total_bar_width as f64 * 0.1
                {
                    is_collecting_cycle = true;
                    if !current_cycle_data.is_empty() {
                        let n = current_cycle_data.len();
                        cycle_samples.push(std::mem::take(&mut current_cycle_data));
                        log::info!(
                            "calibration: captured cycle {}/{} ({} samples)",
                            cycle_samples.len(),
                            CALIBRATION_CYCLES,
                            n,
                        );
                    } else {
                        log::info!(
                            "calibration: detected full->empty wrap (prev={previous}, cur={current}); \
                             began collecting cycle data"
                        );
                    }
                }
            }

            if is_collecting_cycle && cycle_samples.len() < CALIBRATION_CYCLES {
                current_cycle_data.push(current);
            }

            let progress_percent =
                progress.update(current, cycle_samples.len(), is_collecting_cycle);
            last_progress = progress_percent;
            state.update_ui(|ui, _| {
                ui.mode = crate::ui_state::OverlayMode::Calibrating;
                ui.progress_percent = progress_percent;
                ui.cursor_blocked = false;
                ui.message.clear();
            });
            previous_cost_state_raw = Some(current);
        } else {
            none_streak += 1;
            previous_cost_state_raw = None;
        }

        // Diagnostic logging (throttled). The collection loop is otherwise
        // silent, so these lines are the only window into *why* progress may be
        // stuck: a bar that reads `None`/empty every frame points at a wrong
        // ROI, a resolution/format mismatch, or simply not being in battle.
        if frames_seen <= 5 || frames_seen % 30 == 0 {
            match current_cost_state_raw {
                Some(c) => log::debug!(
                    "calibration: frame#{frames_seen} width={c}/{total_bar_width} \
                     collecting={is_collecting_cycle} cycles={}/{} progress={last_progress:.1}% \
                     (some={some_count})",
                    cycle_samples.len(),
                    CALIBRATION_CYCLES,
                ),
                None => log::debug!(
                    "calibration: frame#{frames_seen} width=None (bar not detected) \
                     none_streak={none_streak} (some={some_count})"
                ),
            }
        }
        if none_streak == 60 || (none_streak > 60 && none_streak % 120 == 0) {
            log::warn!(
                "calibration: cost bar not detected for {none_streak} consecutive frames — \
                 check that you are in battle and that ROI/resolution match (roi x1={}, x2={})",
                roi.0,
                roi.1,
            );
        }

        // Ack the frame we just processed.
        if let Err(err) = pipe.ack(frame.id) {
            return Err(format!("calibration: ack failed: {err}"));
        }

        if cycle_samples.len() < CALIBRATION_CYCLES {
            frame = pipe
                .recv_frame()
                .map_err(|e| format!("calibration: recv: {e}"))?;
        }
    }

    state.update_ui(|ui, _| {
        ui.mode = crate::ui_state::OverlayMode::Calibrating;
        ui.progress_percent = 100.0;
        ui.cursor_blocked = false;
    });
    Ok((cycle_samples, screen_width, screen_height, total_bar_width))
}

/// Infer calibration from collected samples and return the calibration data.
/// This is a convenience wrapper around
/// [`infer_calibration_from_samples_with_ui_scaler_and_total_bar_width`].
pub fn infer_calibration(
    cycle_samples: &[Vec<i32>],
    screen_width: u32,
    screen_height: u32,
    ui_scaler: f64,
    total_bar_width: i32,
    calibration_time: f64,
) -> Result<ruler_core::analysis::calibration::CalibrationData, String> {
    infer_calibration_from_samples_with_ui_scaler_and_total_bar_width(
        cycle_samples,
        screen_width,
        screen_height,
        ui_scaler,
        total_bar_width,
        calibration_time,
    )
}

// ---------------------------------------------------------------------------
// CalibrationProgress — reused from the old worker.rs, unchanged in behavior
// ---------------------------------------------------------------------------

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

#[cfg(test)]
mod tests {
    use super::CalibrationProgress;

    fn assert_near(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.001,
            "expected {expected}, got {actual}"
        );
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
