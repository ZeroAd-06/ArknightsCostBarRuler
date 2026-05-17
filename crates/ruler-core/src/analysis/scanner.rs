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

#[inline(always)]
fn is_pixel_grayscale(r: u8, g: u8, b: u8) -> bool {
    (r as i16 - g as i16).unsigned_abs() <= GRAY_TOLERANCE as u16
        && (g as i16 - b as i16).unsigned_abs() <= GRAY_TOLERANCE as u16
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

    let bytes_per_pixel = match format {
        PixelFormat::Rgba => 4,
        PixelFormat::Bgr => 3,
    };

    let stride = width as usize * bytes_per_pixel as usize;
    let buffer_row = (height as i32 - 1 - y_mid) as usize;
    if buffer_row >= height as usize {
        return None;
    }

    let row_offset = buffer_row * stride;
    let read_pixel = |x: i32| -> (u8, u8, u8, u8) {
        let px = x as usize;
        if px >= width as usize {
            return (0, 0, 0, 0);
        }
        let offset = row_offset + px * bytes_per_pixel as usize;
        if offset + bytes_per_pixel as usize > buffer.len() {
            return (0, 0, 0, 0);
        }
        match format {
            PixelFormat::Rgba => {
                let r = buffer[offset];
                let g = buffer[offset + 1];
                let b = buffer[offset + 2];
                let a = buffer[offset + 3];
                (r, g, b, a)
            }
            PixelFormat::Bgr => {
                let b = buffer[offset];
                let g = buffer[offset + 1];
                let r = buffer[offset + 2];
                (r, g, b, ALPHA_OPAQUE)
            }
        }
    };

    let (r_end, g_end, b_end, a_end) = read_pixel(x2 - 1);
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
            let (r, g, b, a) = read_pixel(x);
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
                    let (r, g, b, a) = read_pixel(x);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rgba_buffer(
        width: u32,
        height: u32,
        fill_fn: &dyn Fn(u32, u32) -> [u8; 4],
    ) -> Vec<u8> {
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
}
