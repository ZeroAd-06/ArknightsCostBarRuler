/// ROI calculation for Arknights cost bar.
/// Ported from Python utils.py::find_cost_bar_roi()

const REF_WIDTH: f64 = 1920.0;
const REF_HEIGHT: f64 = 1080.0;
const REF_ASPECT_RATIO: f64 = REF_WIDTH / REF_HEIGHT;

const X1_OFFSET_FROM_RIGHT_REF: f64 = REF_WIDTH - 1739.0;
const X2_OFFSET_FROM_RIGHT_REF: f64 = REF_WIDTH - 1919.0;
const Y1_OFFSET_FROM_BOTTOM_REF: f64 = REF_HEIGHT - 810.0;
const Y2_OFFSET_FROM_BOTTOM_REF: f64 = REF_HEIGHT - 817.0;

pub type Roi = (i32, i32, i32);

pub fn find_cost_bar_roi(screen_width: i32, screen_height: i32) -> Roi {
    let current_aspect_ratio = screen_width as f64 / screen_height as f64;

    let scale = if current_aspect_ratio >= REF_ASPECT_RATIO {
        screen_height as f64 / REF_HEIGHT
    } else {
        screen_width as f64 / REF_WIDTH
    };

    let x1 = screen_width as f64 - X1_OFFSET_FROM_RIGHT_REF * scale;
    let x2 = screen_width as f64 - X2_OFFSET_FROM_RIGHT_REF * scale;
    let y1 = screen_height as f64 - Y1_OFFSET_FROM_BOTTOM_REF * scale;
    let y2 = screen_height as f64 - Y2_OFFSET_FROM_BOTTOM_REF * scale;

    let x1_int = x1.round() as i32;
    let x2_int = x2.round() as i32;
    let y_mid_int = ((y1 + y2) / 2.0).round() as i32;

    (x1_int, x2_int, y_mid_int)
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
}
