/// ROI calculation for Arknights cost bar.
/// Ported from Python utils.py::find_cost_bar_roi()

const REF_WIDTH: f64 = 1920.0;
const REF_HEIGHT: f64 = 1080.0;
const REF_ASPECT_RATIO: f64 = REF_WIDTH / REF_HEIGHT;
pub const DEFAULT_UI_SCALER: f64 = 1.0;

const X1_OFFSET_FROM_RIGHT_REF: f64 = REF_WIDTH - 1739.0;
const X2_OFFSET_FROM_RIGHT_REF: f64 = REF_WIDTH - 1919.0;
const Y1_OFFSET_FROM_BOTTOM_REF: f64 = REF_HEIGHT - 810.0;
const Y2_OFFSET_FROM_BOTTOM_REF: f64 = REF_HEIGHT - 817.0;

pub type Roi = (i32, i32, i32);

pub fn find_cost_bar_roi(screen_width: i32, screen_height: i32) -> Roi {
    find_cost_bar_roi_with_ui_scaler(screen_width, screen_height, DEFAULT_UI_SCALER)
}

pub fn find_cost_bar_roi_with_ui_scaler(
    screen_width: i32,
    screen_height: i32,
    ui_scaler: f64,
) -> Roi {
    let scale = reference_scale(screen_width, screen_height);
    let edge_scale = ui_edge_scale(ui_scaler);

    let x2 = screen_width as f64 - X2_OFFSET_FROM_RIGHT_REF * scale;
    let width = cost_bar_width_frac_with_ui_scaler(screen_width, screen_height, ui_scaler);
    let x1 = x2 - width;
    let y1 = screen_height as f64 - Y1_OFFSET_FROM_BOTTOM_REF * scale * edge_scale;
    let y2 = screen_height as f64 - Y2_OFFSET_FROM_BOTTOM_REF * scale * edge_scale;

    let x1_int = x1.round() as i32;
    let x2_int = x2.round() as i32;
    let y_mid_int = ((y1 + y2) / 2.0).round() as i32;

    (x1_int, x2_int, y_mid_int)
}

/// Reference-resolution scale factor: the cost bar's size relative to the
/// 1920×1080 reference, picked along whichever axis is limiting for the current
/// aspect ratio. Single source of truth shared by the ROI and the fractional
/// width below.
fn reference_scale(screen_width: i32, screen_height: i32) -> f64 {
    let current_aspect_ratio = screen_width as f64 / screen_height as f64;
    if current_aspect_ratio >= REF_ASPECT_RATIO {
        screen_height as f64 / REF_HEIGHT
    } else {
        screen_width as f64 / REF_WIDTH
    }
}

/// Sub-pixel cost-bar width (`x2 - x1`) *before* rounding.
///
/// [`find_cost_bar_roi_with_ui_scaler`] rounds `x1`/`x2` to integer pixels,
/// discarding up to ~1px of the true bar width. Calibration synthesis needs
/// that fractional width to compute the bar length `L = BAR_LENGTH_RATIO ·
/// (x2 - x1)`; rounding first shifts a few frames across a `round()` boundary
/// and makes inference pick an absurd N (e.g. ~134 instead of 30) at
/// resolutions where the width is non-integer (ui_scaler < 1.0, or widths that
/// are not multiples of 60). When the geometry is integral this equals the
/// rounded `x2 - x1`, so reference resolutions are unaffected.
pub fn cost_bar_width_frac_with_ui_scaler(
    screen_width: i32,
    screen_height: i32,
    ui_scaler: f64,
) -> f64 {
    let scale = reference_scale(screen_width, screen_height);
    let edge_scale = ui_edge_scale(ui_scaler);
    (X1_OFFSET_FROM_RIGHT_REF - X2_OFFSET_FROM_RIGHT_REF) * scale * edge_scale
}

pub fn ui_edge_scale(ui_scaler: f64) -> f64 {
    let ui_scaler = if ui_scaler.is_finite() {
        ui_scaler.clamp(0.0, 1.0)
    } else {
        DEFAULT_UI_SCALER
    };
    0.9 + 0.1 * ui_scaler
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roi_1080p() {
        let (x1, x2, y) = find_cost_bar_roi(1920, 1080);
        assert_eq!(x1, 1739);
        assert_eq!(x2, 1919);
        assert!(y > 800 && y < 830, "y_mid at 1080p: {y}");
    }

    #[test]
    fn test_roi_720p() {
        let (x1, x2, _y) = find_cost_bar_roi(1280, 720);
        assert!(x1 > 1100 && x1 < 1200, "x1 at 720p: {x1}");
        assert!(x2 > 1270 && x2 < 1290, "x2 at 720p: {x2}");
    }

    #[test]
    fn test_roi_ultrawide() {
        let (x1, x2, _y) = find_cost_bar_roi(2560, 1080);
        assert_eq!(x1, 2560 - 181);
        assert_eq!(x2, 2560 - 1);
    }

    #[test]
    fn ui_scaler_shrinks_from_right_and_bottom_anchors() {
        let full = find_cost_bar_roi_with_ui_scaler(1920, 1080, 1.0);
        let compact = find_cost_bar_roi_with_ui_scaler(1920, 1080, 0.0);

        assert_eq!(full, (1739, 1919, 814));
        assert_eq!(compact, (1757, 1919, 840));
    }

    #[test]
    fn fractional_width_preserves_subpixel_for_non_integer_geometry() {
        // 2558×1440 at ui_scaler=0.0 (edge_scale=0.9): the true bar width is
        // ~215.83px but the rounded ROI reports 216. Calibration must use the
        // fractional value or it infers an absurd N (~134 instead of 30).
        let frac = cost_bar_width_frac_with_ui_scaler(2558, 1440, 0.0);
        assert!((frac - 215.832).abs() < 0.01, "frac width: {frac}");

        let (x1, x2, _) = find_cost_bar_roi_with_ui_scaler(2558, 1440, 0.0);
        assert_eq!(x2 - x1, 216);
    }

    #[test]
    fn fractional_width_matches_integer_width_at_reference_resolutions() {
        // 720p/1080p/21:9-1440p at ui_scaler=1.0 all yield integral widths
        // (120/180/240), which is why the off-by-one bug never surfaced there.
        for (w, h) in [(1280, 720), (1920, 1080), (3440, 1440)] {
            let frac = cost_bar_width_frac_with_ui_scaler(w, h, 1.0);
            assert!(
                (frac - frac.round()).abs() < 1e-9,
                "{w}x{h} expected integral frac width, got {frac}"
            );
        }
    }
}
