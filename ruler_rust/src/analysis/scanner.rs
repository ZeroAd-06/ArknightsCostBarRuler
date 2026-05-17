/// Zero-copy pixel scanner for cost bar analysis.
/// Operates directly on raw RGBA/BGR buffers - NO PIL conversion needed.
/// Ported from Python utils.py::_get_raw_filled_pixel_width()

use super::roi::Roi;

/// Pixel format of the raw capture buffer
#[derive(Clone, Copy, Debug)]
pub enum PixelFormat {
    /// 4 bytes per pixel: R, G, B, A (MuMu native format, bottom-up)
    Rgba,
    /// 3 bytes per pixel: B, G, R (LDPlayer/Windows native format, bottom-up)
    Bgr,
}

/// Normal mode thresholds
const WHITE_THRESHOLD: u8 = 250;
/// Masked mode thresholds
const MASKED_WHITE_THRESHOLD: u8 = 150;
const MASKED_MAX_BRIGHTNESS: u8 = 165;
/// General constants
const GRAY_TOLERANCE: u8 = 20;
const ALPHA_OPAQUE: u8 = 255;

/// Check if a pixel is grayscale (R ≈ G ≈ B within tolerance)
#[inline(always)]
fn is_pixel_grayscale(r: u8, g: u8, b: u8) -> bool {
    (r as i16 - g as i16).unsigned_abs() <= GRAY_TOLERANCE as u16
        && (g as i16 - b as i16).unsigned_abs() <= GRAY_TOLERANCE as u16
}

/// Extract filled pixel width from cost bar ROI in a raw pixel buffer.
/// This is the HOT PATH - called every frame, must be as fast as possible.
///
/// # Arguments
/// * `buffer` - Raw pixel data (RGBA or BGR format)
/// * `width` - Image width in pixels
/// * `height` - Image height in pixels
/// * `format` - Pixel format (RGBA or BGR)
/// * `roi` - (x1, x2, y_mid) region of interest
///
/// # Returns
/// * `Some(pixel_width)` - Filled pixel width of the cost bar
/// * `None` - ROI is invalid or no cost bar detected
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

    // Calculate stride (row bytes). Some capture methods may have padding.
    let stride = width as usize * bytes_per_pixel as usize;

    // MuMu and LDPlayer capture bottom-up, so we need to flip Y.
    // In a bottom-up buffer, row 0 in image space = last row in buffer.
    // y_mid=0 in image = row (height-1) in buffer, y_mid=height-1 = row 0.
    // So buffer_row = height - 1 - y_mid
    let buffer_row = (height as i32 - 1 - y_mid) as usize;
    if buffer_row >= height as usize {
        return None;
    }

    let row_offset = buffer_row * stride;

    // Helper to read pixel at x coordinate
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
                (r, g, b, ALPHA_OPAQUE) // BGR always opaque
            }
        }
    };

    // --- Quick ROI validity check: end pixel ---
    let (r_end, g_end, b_end, a_end) = read_pixel(x2 - 1);
    if a_end != ALPHA_OPAQUE || !is_pixel_grayscale(r_end, g_end, b_end) {
        return None;
    }

    // --- Normal brightness cost bar detection ---
    let mut filled_width: i32 = 0;
    let is_end_pixel_white = r_end > WHITE_THRESHOLD && g_end > WHITE_THRESHOLD && b_end > WHITE_THRESHOLD;

    if is_end_pixel_white {
        filled_width = total_width;
    } else {
        // Scan from right to left to find the fill edge
        for x in (x1..(x2 - 1)).rev() {
            let (r, g, b, a) = read_pixel(x);
            if a != ALPHA_OPAQUE || !is_pixel_grayscale(r, g, b) {
                return None; // Invalid pixel, not a cost bar
            }
            if r > WHITE_THRESHOLD && g > WHITE_THRESHOLD && b > WHITE_THRESHOLD {
                filled_width = x - x1 + 1;
                break;
            }
        }
    }

    // --- Fallback to masked mode detection ---
    if filled_width == 0 {
        // If end pixel is too bright, can't be masked mode
        if r_end > MASKED_MAX_BRIGHTNESS || g_end > MASKED_MAX_BRIGHTNESS || b_end > MASKED_MAX_BRIGHTNESS {
            // Not masked mode, width stays 0
        } else {
            // Try masked mode
            let is_end_pixel_masked_white = r_end > MASKED_WHITE_THRESHOLD
                && g_end > MASKED_WHITE_THRESHOLD
                && b_end > MASKED_WHITE_THRESHOLD;

            if is_end_pixel_masked_white {
                filled_width = total_width;
            } else {
                // Scan with masked thresholds
                for x in (x1..(x2 - 1)).rev() {
                    let (r, g, b, a) = read_pixel(x);
                    // In masked mode, any pixel too bright invalidates it
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

    fn make_rgba_buffer(width: u32, height: u32, fill_fn: &dyn Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut buf = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let [r, g, b, a] = fill_fn(x, y);
                let offset = ((height - 1 - y) * width * 4 + x * 4) as usize; // bottom-up
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
        let roi = (10, 190, 50); // x1=10, x2=190, y_mid=50
        let result = get_raw_filled_pixel_width(&buf, w, h, PixelFormat::Rgba, roi);
        assert_eq!(result, Some(180)); // total_width = 190-10 = 180
    }

    #[test]
    fn test_all_black_bar() {
        let w = 200u32;
        let h = 100u32;
        let buf = make_rgba_buffer(w, h, &|_x, _y| [30, 30, 30, 255]);
        let roi = (10, 190, 50);
        let result = get_raw_filled_pixel_width(&buf, w, h, PixelFormat::Rgba, roi);
        assert_eq!(result, Some(0)); // No white pixels found
    }

    #[test]
    fn test_half_filled_bar() {
        let w = 200u32;
        let h = 100u32;
        let buf = make_rgba_buffer(w, h, &|x, _y| {
            if x < 100 {
                [252, 252, 252, 255] // left half: white (filled)
            } else {
                [30, 30, 30, 255] // right half: dark (empty)
            }
        });
        let roi = (10, 190, 50);
        let result = get_raw_filled_pixel_width(&buf, w, h, PixelFormat::Rgba, roi);
        // Scanning from right (x=188) toward left, first white pixel is at x=99
        // filled_width = 99 - 10 + 1 = 90
        assert!(result.is_some());
        let fw = result.unwrap();
        assert!(fw > 80 && fw < 100, "Half-filled bar: {fw}");
    }

    #[test]
    fn test_invalid_roi_non_opaque() {
        let w = 200u32;
        let h = 100u32;
        let buf = make_rgba_buffer(w, h, &|_x, _y| [252, 252, 252, 0]); // transparent
        let roi = (10, 190, 50);
        let result = get_raw_filled_pixel_width(&buf, w, h, PixelFormat::Rgba, roi);
        assert_eq!(result, None);
    }

    #[test]
    fn test_bgr_format() {
        let w = 200u32;
        let h = 100u32;
        // BGR buffer, 3 bytes per pixel, bottom-up
        let mut buf = vec![0u8; (w * h * 3) as usize];
        for y in 0..h {
            for x in 0..w {
                let offset = ((h - 1 - y) * w * 3 + x * 3) as usize;
                // BGR: B=252, G=252, R=252 → white
                buf[offset] = 252;     // B
                buf[offset + 1] = 252; // G
                buf[offset + 2] = 252; // R
            }
        }
        let roi = (10, 190, 50);
        let result = get_raw_filled_pixel_width(&buf, w, h, PixelFormat::Bgr, roi);
        assert_eq!(result, Some(180));
    }
}
