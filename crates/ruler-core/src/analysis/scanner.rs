/// Zero-copy pixel scanner for cost bar analysis.
/// Operates directly on raw RGBA/BGR buffers.
use super::roi::Roi;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    Rgba,
    Bgr,
}

const WHITE_THRESHOLD: u8 = 250;
const MASKED_WHITE_THRESHOLD: u8 = 150;
const MASKED_MAX_BRIGHTNESS: u8 = 165;
const GRAY_TOLERANCE: u8 = 20;
const ALPHA_OPAQUE: u8 = 255;

const COST_SIGN_REF_WIDTH: f64 = 1280.0;
const COST_SIGN_REF_HEIGHT: f64 = 720.0;
const COST_SIGN_REF_ASPECT_RATIO: f64 = COST_SIGN_REF_WIDTH / COST_SIGN_REF_HEIGHT;

const COST_SIGN_LEFT_OFFSET_FROM_RIGHT_REF: f64 = 70.0;
const COST_SIGN_RIGHT_OFFSET_FROM_RIGHT_REF: f64 = 42.0;
const COST_SIGN_TOP_OFFSET_FROM_BOTTOM_REF: f64 = 208.0;
const COST_SIGN_BOTTOM_OFFSET_FROM_BOTTOM_REF: f64 = 201.0;
const COST_SIGN_MIN_RUN_REF: f64 = 14.0;
const COST_SIGN_MIN_ROWS_REF: f64 = 2.0;

#[inline(always)]
fn read_pixel(
    buffer: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    x: i32,
    y: i32,
) -> Option<(u8, u8, u8, u8)> {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        return None;
    }

    let bytes_per_pixel = match format {
        PixelFormat::Rgba => 4,
        PixelFormat::Bgr => 3,
    };
    let stride = width as usize * bytes_per_pixel;
    let buffer_row = (height as i32 - 1 - y) as usize;
    let offset = buffer_row * stride + x as usize * bytes_per_pixel;
    if offset + bytes_per_pixel > buffer.len() {
        return None;
    }

    match format {
        PixelFormat::Rgba => {
            let r = buffer[offset];
            let g = buffer[offset + 1];
            let b = buffer[offset + 2];
            let a = buffer[offset + 3];
            Some((r, g, b, a))
        }
        PixelFormat::Bgr => {
            let b = buffer[offset];
            let g = buffer[offset + 1];
            let r = buffer[offset + 2];
            Some((r, g, b, ALPHA_OPAQUE))
        }
    }
}

#[inline(always)]
fn is_pixel_grayscale(r: u8, g: u8, b: u8) -> bool {
    (r as i16 - g as i16).unsigned_abs() <= GRAY_TOLERANCE as u16
        && (g as i16 - b as i16).unsigned_abs() <= GRAY_TOLERANCE as u16
}

#[inline(always)]
fn is_cost_sign_pixel(r: u8, g: u8, b: u8, a: u8) -> bool {
    if a != ALPHA_OPAQUE {
        return false;
    }

    let max_channel = r.max(g).max(b);
    let min_channel = r.min(g).min(b);
    max_channel >= 175 && min_channel >= 145 && max_channel - min_channel <= 80
}

#[inline]
fn cost_sign_scale(width: u32, height: u32) -> f64 {
    let aspect_ratio = width as f64 / height as f64;
    if aspect_ratio >= COST_SIGN_REF_ASPECT_RATIO {
        height as f64 / COST_SIGN_REF_HEIGHT
    } else {
        width as f64 / COST_SIGN_REF_WIDTH
    }
}

#[inline]
fn cost_sign_scan_rect(width: u32, height: u32) -> Option<(i32, i32, i32, i32, f64)> {
    if width == 0 || height == 0 {
        return None;
    }

    let scale = cost_sign_scale(width, height);
    let left = (width as f64 - COST_SIGN_LEFT_OFFSET_FROM_RIGHT_REF * scale).round() as i32;
    let right = (width as f64 - COST_SIGN_RIGHT_OFFSET_FROM_RIGHT_REF * scale).round() as i32;
    let top = (height as f64 - COST_SIGN_TOP_OFFSET_FROM_BOTTOM_REF * scale).round() as i32;
    let bottom = (height as f64 - COST_SIGN_BOTTOM_OFFSET_FROM_BOTTOM_REF * scale).round() as i32;

    let left = left.clamp(0, width as i32);
    let right = right.clamp(0, width as i32);
    let top = top.clamp(0, height as i32);
    let bottom = bottom.clamp(0, height as i32);
    if left >= right || top >= bottom {
        return None;
    }

    Some((left, right, top, bottom, scale))
}

pub fn get_raw_filled_pixel_width(
    buffer: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    roi: Roi,
) -> Option<i32> {
    let (x1, x2, y_mid) = roi;
    let total_width = x2 - x1;
    if total_width <= 0 {
        return None;
    }

    let (r_end, g_end, b_end, a_end) = read_pixel(buffer, width, height, format, x2 - 1, y_mid)?;
    if a_end != ALPHA_OPAQUE || !is_pixel_grayscale(r_end, g_end, b_end) {
        return None;
    }

    let mut filled_width: i32 = 0;
    let is_end_pixel_white =
        r_end > WHITE_THRESHOLD && g_end > WHITE_THRESHOLD && b_end > WHITE_THRESHOLD;

    if is_end_pixel_white {
        filled_width = total_width;
    } else {
        for x in (x1..(x2 - 1)).rev() {
            let (r, g, b, a) = read_pixel(buffer, width, height, format, x, y_mid)?;
            if a != ALPHA_OPAQUE || !is_pixel_grayscale(r, g, b) {
                return None;
            }
            if r > WHITE_THRESHOLD && g > WHITE_THRESHOLD && b > WHITE_THRESHOLD {
                filled_width = x - x1 + 1;
                break;
            }
        }
    }

    if filled_width == 0 {
        if r_end > MASKED_MAX_BRIGHTNESS
            || g_end > MASKED_MAX_BRIGHTNESS
            || b_end > MASKED_MAX_BRIGHTNESS
        {
        } else {
            let is_end_pixel_masked_white = r_end > MASKED_WHITE_THRESHOLD
                && g_end > MASKED_WHITE_THRESHOLD
                && b_end > MASKED_WHITE_THRESHOLD;

            if is_end_pixel_masked_white {
                filled_width = total_width;
            } else {
                for x in (x1..(x2 - 1)).rev() {
                    let (r, g, b, a) = read_pixel(buffer, width, height, format, x, y_mid)?;
                    if a != ALPHA_OPAQUE
                        || !is_pixel_grayscale(r, g, b)
                        || r > MASKED_MAX_BRIGHTNESS
                        || g > MASKED_MAX_BRIGHTNESS
                        || b > MASKED_MAX_BRIGHTNESS
                    {
                        filled_width = 0;
                        break;
                    }
                    if r > MASKED_WHITE_THRESHOLD
                        && g > MASKED_WHITE_THRESHOLD
                        && b > MASKED_WHITE_THRESHOLD
                    {
                        filled_width = x - x1 + 1;
                        break;
                    }
                }
            }
        }
    }

    Some(filled_width)
}

pub fn is_cost_negative(buffer: &[u8], width: u32, height: u32, format: PixelFormat) -> bool {
    let Some((left, right, top, bottom, scale)) = cost_sign_scan_rect(width, height) else {
        return false;
    };

    let min_run = (COST_SIGN_MIN_RUN_REF * scale).round().max(6.0) as i32;
    let min_rows = (COST_SIGN_MIN_ROWS_REF * scale).round().max(1.0) as i32;
    let mut matching_rows = 0;

    for y in top..bottom {
        let mut current_run = 0;
        let mut row_matches = false;
        for x in left..right {
            let is_lit = read_pixel(buffer, width, height, format, x, y)
                .map(|(r, g, b, a)| is_cost_sign_pixel(r, g, b, a))
                .unwrap_or(false);
            if is_lit {
                current_run += 1;
                if current_run >= min_run {
                    row_matches = true;
                    break;
                }
            } else {
                current_run = 0;
            }
        }

        if row_matches {
            matching_rows += 1;
            if matching_rows >= min_rows {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rgba_buffer(width: u32, height: u32, fill_fn: &dyn Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut buf = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let [r, g, b, a] = fill_fn(x, y);
                let offset = ((height - 1 - y) * width * 4 + x * 4) as usize;
                buf[offset] = r;
                buf[offset + 1] = g;
                buf[offset + 2] = b;
                buf[offset + 3] = a;
            }
        }
        buf
    }

    fn put_bgr_screen_pixel(buf: &mut [u8], width: u32, height: u32, x: u32, y: u32, rgb: [u8; 3]) {
        let offset = ((height - 1 - y) * width * 3 + x * 3) as usize;
        buf[offset] = rgb[2];
        buf[offset + 1] = rgb[1];
        buf[offset + 2] = rgb[0];
    }

    #[test]
    fn test_all_white_bar() {
        let w = 200u32;
        let h = 100u32;
        let buf = make_rgba_buffer(w, h, &|_x, _y| [252, 252, 252, 255]);
        let roi = (10, 190, 50);
        let result = get_raw_filled_pixel_width(&buf, w, h, PixelFormat::Rgba, roi);
        assert_eq!(result, Some(180));
    }

    #[test]
    fn test_all_black_bar() {
        let w = 200u32;
        let h = 100u32;
        let buf = make_rgba_buffer(w, h, &|_x, _y| [30, 30, 30, 255]);
        let roi = (10, 190, 50);
        let result = get_raw_filled_pixel_width(&buf, w, h, PixelFormat::Rgba, roi);
        assert_eq!(result, Some(0));
    }

    #[test]
    fn test_half_filled_bar() {
        let w = 200u32;
        let h = 100u32;
        let buf = make_rgba_buffer(w, h, &|x, _y| {
            if x < 100 {
                [252, 252, 252, 255]
            } else {
                [30, 30, 30, 255]
            }
        });
        let roi = (10, 190, 50);
        let result = get_raw_filled_pixel_width(&buf, w, h, PixelFormat::Rgba, roi);
        assert!(result.is_some());
        let fw = result.unwrap();
        assert!(fw > 80 && fw < 100, "Half-filled bar: {fw}");
    }

    #[test]
    fn test_invalid_roi_non_opaque() {
        let w = 200u32;
        let h = 100u32;
        let buf = make_rgba_buffer(w, h, &|_x, _y| [252, 252, 252, 0]);
        let roi = (10, 190, 50);
        let result = get_raw_filled_pixel_width(&buf, w, h, PixelFormat::Rgba, roi);
        assert_eq!(result, None);
    }

    #[test]
    fn test_bgr_format() {
        let w = 200u32;
        let h = 100u32;
        let mut buf = vec![0u8; (w * h * 3) as usize];
        for y in 0..h {
            for x in 0..w {
                let offset = ((h - 1 - y) * w * 3 + x * 3) as usize;
                buf[offset] = 252;
                buf[offset + 1] = 252;
                buf[offset + 2] = 252;
            }
        }
        let roi = (10, 190, 50);
        let result = get_raw_filled_pixel_width(&buf, w, h, PixelFormat::Bgr, roi);
        assert_eq!(result, Some(180));
    }

    #[test]
    fn detects_negative_cost_sign() {
        let width = 1280;
        let height = 720;
        let mut buf = vec![20u8; (width * height * 3) as usize];
        for y in 514..517 {
            for x in 1210..1231 {
                put_bgr_screen_pixel(&mut buf, width, height, x, y, [255, 255, 255]);
            }
        }

        assert!(is_cost_negative(&buf, width, height, PixelFormat::Bgr));
    }

    #[test]
    fn ignores_positive_digit_strokes_near_sign_area() {
        let width = 1280;
        let height = 720;
        let mut buf = vec![20u8; (width * height * 3) as usize];
        for y in 506..535 {
            for x in 1228..1232 {
                put_bgr_screen_pixel(&mut buf, width, height, x, y, [255, 255, 255]);
            }
        }
        for y in 521..524 {
            for x in 1220..1238 {
                put_bgr_screen_pixel(&mut buf, width, height, x, y, [255, 255, 255]);
            }
        }

        assert!(!is_cost_negative(&buf, width, height, PixelFormat::Bgr));
    }
}
