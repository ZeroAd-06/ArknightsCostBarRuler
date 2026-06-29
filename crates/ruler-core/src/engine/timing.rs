use crate::analysis::roi;

pub(crate) fn normalized_ui_scaler(ui_scaler: f64) -> f64 {
    if ui_scaler.is_finite() {
        ui_scaler.clamp(0.0, 1.0)
    } else {
        roi::DEFAULT_UI_SCALER
    }
}

pub(crate) fn rounded_frame_count(frames: f64) -> i32 {
    frames.round() as i32
}
