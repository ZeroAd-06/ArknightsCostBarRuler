//! Pure cost-bar timing math: cycle/endpoint resolution and pixel→frame mapping.
//!
//! These functions are stateless; the stateful [`super::Analyzer`] owns the phase
//! anchor and elapsed-frame accumulation and drives them.
use super::{CycleEndpointMode, CycleTiming, FrameLookup, PhaseSample};
use crate::analysis::calibration::{CalibrationTimingModel, LoadedCalibration};
use crate::analysis::mapping::CalibrationTable;
use crate::analysis::roi;

const NEGATIVE_COST_INTERVAL_MULTIPLIER: i32 = 2;

pub(crate) fn effective_total_frames(total_frames: i32, cost_is_negative: bool) -> i32 {
    if cost_is_negative {
        total_frames.saturating_mul(NEGATIVE_COST_INTERVAL_MULTIPLIER)
    } else {
        total_frames
    }
}

pub(crate) fn current_cycle_timing(
    calibration: &LoadedCalibration,
    base_profile: usize,
    cycle_counter: usize,
) -> CycleTiming {
    let num_profiles = calibration.tables.len();
    let profile_index = (base_profile + cycle_counter) % num_profiles;
    let base_total_frames = calibration.tables[profile_index].total_frames;

    let CalibrationTimingModel::OpenInteriorV1 {
        total_bar_width,
        boundary_switch_frame,
    } = calibration.timing_model;
    let boundary_cycle_index =
        boundary_cycle_index(calibration, base_profile, boundary_switch_frame);
    let endpoint_mode = if cycle_counter < boundary_cycle_index {
        CycleEndpointMode::LeftClosedRightOpen
    } else if cycle_counter == boundary_cycle_index {
        CycleEndpointMode::LeftClosedRightClosed
    } else {
        CycleEndpointMode::LeftOpenRightClosed
    };
    CycleTiming {
        profile_index,
        total_bar_width,
        total_frames: if endpoint_mode == CycleEndpointMode::LeftClosedRightClosed {
            base_total_frames.saturating_add(1)
        } else {
            base_total_frames
        },
        endpoint_mode,
    }
}

pub(crate) fn boundary_cycle_index(
    calibration: &LoadedCalibration,
    base_profile: usize,
    boundary_switch_frame: i32,
) -> usize {
    let num_profiles = calibration.tables.len();
    if num_profiles == 0 {
        return 0;
    }

    let mut elapsed_frames = 0i32;
    let target_frame = boundary_switch_frame.max(0);
    for cycle_index in 0usize.. {
        let profile_index = (base_profile + cycle_index) % num_profiles;
        let total_frames = calibration.tables[profile_index].total_frames.max(1);
        let next_elapsed = elapsed_frames.saturating_add(total_frames);
        if target_frame < next_elapsed {
            return cycle_index;
        }
        elapsed_frames = next_elapsed;
    }

    0
}

pub(crate) fn normalized_ui_scaler(ui_scaler: f64) -> f64 {
    if ui_scaler.is_finite() {
        ui_scaler.clamp(0.0, 1.0)
    } else {
        roi::DEFAULT_UI_SCALER
    }
}

pub(crate) fn lookup_bar_frame(
    table: &CalibrationTable,
    cycle_timing: CycleTiming,
    pixel_width: i32,
    cost_is_negative: bool,
) -> Option<FrameLookup> {
    if cycle_timing.total_frames <= 0 {
        return None;
    }
    lookup_open_interior_bar_frame(table, cycle_timing, pixel_width, cost_is_negative)
}

fn lookup_open_interior_bar_frame(
    table: &CalibrationTable,
    cycle_timing: CycleTiming,
    pixel_width: i32,
    cost_is_negative: bool,
) -> Option<FrameLookup> {
    let display_frame = if cost_is_negative {
        lookup_open_interior_display_frame_f64(table, cycle_timing, pixel_width)?
    } else {
        lookup_open_interior_display_frame(table, cycle_timing, pixel_width)? as f64
    };
    let phase = (display_frame / cycle_timing.total_frames as f64).clamp(0.0, 1.0);
    let logical_frame = if cost_is_negative {
        frame_from_phase(
            phase,
            effective_total_frames(cycle_timing.total_frames, true),
        )
    } else {
        display_frame.round() as i32
    };

    Some(FrameLookup {
        logical_frame,
        phase,
    })
}

fn lookup_open_interior_display_frame(
    table: &CalibrationTable,
    cycle_timing: CycleTiming,
    pixel_width: i32,
) -> Option<i32> {
    if open_interior_excludes_width(cycle_timing, pixel_width) {
        return None;
    }

    open_interior_endpoint_frame(cycle_timing, pixel_width)
        .or_else(|| {
            let internal_frame = table.lookup(pixel_width)?;
            Some(open_interior_internal_frame(
                cycle_timing.endpoint_mode,
                internal_frame,
            ))
        })
        .map(|frame| frame.clamp(0, cycle_timing.total_frames.saturating_sub(1)))
}

fn lookup_open_interior_display_frame_f64(
    table: &CalibrationTable,
    cycle_timing: CycleTiming,
    pixel_width: i32,
) -> Option<f64> {
    if open_interior_excludes_width(cycle_timing, pixel_width) {
        return None;
    }

    open_interior_endpoint_frame(cycle_timing, pixel_width)
        .map(|frame| frame as f64)
        .or_else(|| {
            let internal_frame = table.lookup_interpolated_frame(pixel_width)?;
            Some(open_interior_internal_frame_f64(
                cycle_timing.endpoint_mode,
                internal_frame,
            ))
        })
        .map(|frame| frame.clamp(0.0, cycle_timing.total_frames.saturating_sub(1) as f64))
}

fn open_interior_endpoint_frame(cycle_timing: CycleTiming, pixel_width: i32) -> Option<i32> {
    let total_bar_width = cycle_timing.total_bar_width;

    if matches!(
        cycle_timing.endpoint_mode,
        CycleEndpointMode::LeftClosedRightOpen | CycleEndpointMode::LeftClosedRightClosed
    ) && pixel_width <= 0
    {
        return Some(0);
    }

    if matches!(
        cycle_timing.endpoint_mode,
        CycleEndpointMode::LeftClosedRightClosed | CycleEndpointMode::LeftOpenRightClosed
    ) && pixel_width >= total_bar_width
    {
        return Some(cycle_timing.total_frames.saturating_sub(1));
    }

    None
}

fn open_interior_excludes_width(cycle_timing: CycleTiming, pixel_width: i32) -> bool {
    cycle_timing.endpoint_mode == CycleEndpointMode::LeftClosedRightOpen
        && pixel_width >= cycle_timing.total_bar_width
}

fn open_interior_internal_frame(endpoint_mode: CycleEndpointMode, internal_frame: i32) -> i32 {
    match endpoint_mode {
        CycleEndpointMode::LeftOpenRightClosed => internal_frame,
        CycleEndpointMode::LeftClosedRightOpen | CycleEndpointMode::LeftClosedRightClosed => {
            internal_frame.saturating_add(1)
        }
    }
}

fn open_interior_internal_frame_f64(endpoint_mode: CycleEndpointMode, internal_frame: f64) -> f64 {
    match endpoint_mode {
        CycleEndpointMode::LeftOpenRightClosed => internal_frame,
        CycleEndpointMode::LeftClosedRightOpen | CycleEndpointMode::LeftClosedRightClosed => {
            internal_frame + 1.0
        }
    }
}

fn frame_from_phase(phase: f64, total_frames: i32) -> i32 {
    if total_frames <= 0 {
        return 0;
    }

    let frame = (phase.clamp(0.0, 1.0) * total_frames as f64).round() as i32;
    frame.clamp(0, total_frames - 1)
}

pub(crate) fn is_natural_cycle_wrap(previous: PhaseSample, current_phase: f64) -> bool {
    previous.phase > 0.75 && current_phase < 0.25
}

pub(crate) fn phase_delta(previous: PhaseSample, current: PhaseSample) -> Option<f64> {
    let raw_delta = current.phase - previous.phase;
    if (-0.5..0.0).contains(&raw_delta) {
        return None;
    }

    let mut delta = raw_delta;
    if delta < -0.5 {
        delta += 1.0;
    }
    Some(delta.max(0.0))
}

pub(crate) fn rounded_frame_count(frames: f64) -> i32 {
    frames.round() as i32
}
