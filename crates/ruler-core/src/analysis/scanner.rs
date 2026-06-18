/// Zero-copy pixel scanner for cost bar analysis.
/// Operates directly on raw RGBA/BGR buffers.
use super::roi::Roi;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    Rgba,
    Bgr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BattleState {
    PointTwoXRunning,
    OneXRunning,
    TwoXRunning,
    PointTwoXPaused,
    OneXPaused,
    TwoXPaused,
    BeforeOrAfterBattle,
    NotInBattle,
}

impl BattleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PointTwoXRunning => "0.2x_running",
            Self::OneXRunning => "1x_running",
            Self::TwoXRunning => "2x_running",
            Self::PointTwoXPaused => "0.2x_paused",
            Self::OneXPaused => "1x_paused",
            Self::TwoXPaused => "2x_paused",
            Self::BeforeOrAfterBattle => "before_or_after_battle",
            Self::NotInBattle => "not_in_battle",
        }
    }

    pub fn is_in_battle(self) -> bool {
        matches!(
            self,
            Self::PointTwoXRunning
                | Self::OneXRunning
                | Self::TwoXRunning
                | Self::PointTwoXPaused
                | Self::OneXPaused
                | Self::TwoXPaused
        )
    }
}

const WHITE_THRESHOLD: u8 = 250;
const MASKED_WHITE_THRESHOLD: u8 = 150;
const MASKED_MAX_BRIGHTNESS: u8 = 165;
const GRAY_TOLERANCE: u8 = 20;
const ALPHA_OPAQUE: u8 = 255;
const VALIDITY_LEFT_INSET_PX: i32 = 1;

const COST_SIGN_REF_WIDTH: f64 = 1280.0;
const COST_SIGN_REF_HEIGHT: f64 = 720.0;
const COST_SIGN_REF_ASPECT_RATIO: f64 = COST_SIGN_REF_WIDTH / COST_SIGN_REF_HEIGHT;

const COST_SIGN_LEFT_OFFSET_FROM_RIGHT_REF: f64 = 70.0;
const COST_SIGN_RIGHT_OFFSET_FROM_RIGHT_REF: f64 = 42.0;
const COST_SIGN_TOP_OFFSET_FROM_BOTTOM_REF: f64 = 208.0;
const COST_SIGN_BOTTOM_OFFSET_FROM_BOTTOM_REF: f64 = 201.0;
const COST_SIGN_MIN_RUN_REF: f64 = 14.0;
// At 720p the pure-white core of the minus sign is only a single row tall; it
// thickens with resolution. Keep the floor at one row so 720p still detects.
const COST_SIGN_MIN_ROWS_REF: f64 = 1.0;

const BATTLE_BUTTON_REF_WIDTH: f64 = 1280.0;
const BATTLE_BUTTON_REF_HEIGHT: f64 = 720.0;
const BATTLE_BUTTON_REF_ASPECT_RATIO: f64 = BATTLE_BUTTON_REF_WIDTH / BATTLE_BUTTON_REF_HEIGHT;

const SPEED_GLYPH_LEFT_FROM_RIGHT_REF: f64 = 207.0;
const SPEED_GLYPH_RIGHT_FROM_RIGHT_REF: f64 = 153.0;
const SPEED_GLYPH_TOP_REF: f64 = 28.0;
const SPEED_GLYPH_BOTTOM_REF: f64 = 79.0;

const PAUSE_GLYPH_LEFT_FROM_RIGHT_REF: f64 = 92.0;
const PAUSE_GLYPH_RIGHT_FROM_RIGHT_REF: f64 = 50.0;
const PAUSE_GLYPH_TOP_REF: f64 = 38.0;
const PAUSE_GLYPH_BOTTOM_REF: f64 = 69.0;

const GLYPH_BRIGHT_THRESHOLD: u8 = 180;
const GLYPH_DIM_THRESHOLD: u8 = 120;

// Pause/play glyph, normalised bright-pixel area (per scale²). The running glyph
// (two bars) fills more area than the paused glyph (a single triangle).
const PAUSE_RUNNING_MIN: f64 = 560.0;
const PAUSE_RUNNING_MAX: f64 = 710.0;
const PAUSE_PAUSED_MIN: f64 = 380.0;
const PAUSE_PAUSED_MAX: f64 = 500.0;

// Speed glyph ("1X" / "2X") bright-pixel area; 2X carries more strokes than 1X.
const SPEED_1X_MIN: f64 = 360.0;
const SPEED_1X_MAX: f64 = 520.0;
const SPEED_2X_MIN: f64 = 540.0;
const SPEED_2X_MAX: f64 = 740.0;
// A crisp glyph is bright strokes on a dark button, so it has few mid-tone
// pixels. During deployment slow-mo (0.2x) the speed button is greyed/occluded
// by the deploy-range overlay, flooding the box with mid-tones and pushing
// (dim - bright) far past this limit even when a few bright pixels survive.
const SPEED_GLYPH_CRISP_MAX: f64 = 300.0;

// Before/after battle: both glyphs are dark, but the greyed button outlines
// remain as a moderate band of mid-tone pixels.
const GLYPH_PRESENT_MAX: f64 = 150.0;
const INIT_DIM_MIN: f64 = 150.0;
const INIT_DIM_MAX: f64 = 500.0;

const TAKEOVER_OVERLAY_LEFT_REF: f64 = 315.0;
const TAKEOVER_OVERLAY_RIGHT_REF: f64 = 510.0;
const TAKEOVER_OVERLAY_TOP_REF: f64 = 610.0;
const TAKEOVER_OVERLAY_BOTTOM_REF: f64 = 696.0;
const TAKEOVER_OVERLAY_BRIGHT_THRESHOLD: u8 = 150;
const TAKEOVER_OVERLAY_BRIGHT_MIN: f64 = 350.0;

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

/// The cost minus sign has a solid pure-white (#ffffff) core that is present even
/// at 720p and only grows at higher resolutions. Matching that core specifically
/// — rather than any bright-ish pixel — is what separates the sign from the
/// anti-aliased curves/edges of positive digits: those accumulate wide "bright"
/// runs but never a wide *pure-white* run across the bar's vertical centre.
#[inline(always)]
fn is_cost_sign_pixel(r: u8, g: u8, b: u8, a: u8) -> bool {
    a == ALPHA_OPAQUE && r > WHITE_THRESHOLD && g > WHITE_THRESHOLD && b > WHITE_THRESHOLD
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

#[derive(Clone, Copy, Debug)]
struct Rect {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

impl Rect {
    fn is_empty(self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }
}

#[derive(Clone, Copy, Debug)]
enum SpeedButtonState {
    PointTwoX,
    OneX,
    TwoX,
}

#[derive(Clone, Copy, Debug)]
enum PauseButtonState {
    Running,
    Paused,
}

#[inline]
fn battle_button_scale(width: u32, height: u32) -> f64 {
    let aspect_ratio = width as f64 / height as f64;
    if aspect_ratio >= BATTLE_BUTTON_REF_ASPECT_RATIO {
        height as f64 / BATTLE_BUTTON_REF_HEIGHT
    } else {
        width as f64 / BATTLE_BUTTON_REF_WIDTH
    }
}

#[inline]
fn glyph_rect_from_right(
    width: u32,
    height: u32,
    scale: f64,
    left_from_right_ref: f64,
    right_from_right_ref: f64,
    top_ref: f64,
    bottom_ref: f64,
) -> Rect {
    let left = (width as f64 - left_from_right_ref * scale).round() as i32;
    let right = (width as f64 - right_from_right_ref * scale).round() as i32;
    let top = (top_ref * scale).round() as i32;
    let bottom = (bottom_ref * scale).round() as i32;

    Rect {
        left: left.clamp(0, width as i32),
        right: right.clamp(0, width as i32),
        top: top.clamp(0, height as i32),
        bottom: bottom.clamp(0, height as i32),
    }
}

#[inline]
fn scaled_rect(
    width: u32,
    height: u32,
    scale: f64,
    left_ref: f64,
    right_ref: f64,
    top_ref: f64,
    bottom_ref: f64,
) -> Rect {
    let left = (left_ref * scale).round() as i32;
    let right = (right_ref * scale).round() as i32;
    let top = (top_ref * scale).round() as i32;
    let bottom = (bottom_ref * scale).round() as i32;

    Rect {
        left: left.clamp(0, width as i32),
        right: right.clamp(0, width as i32),
        top: top.clamp(0, height as i32),
        bottom: bottom.clamp(0, height as i32),
    }
}

#[inline]
fn is_bright_enough(r: u8, g: u8, b: u8, threshold: u8) -> bool {
    r as u16 + g as u16 + b as u16 >= threshold as u16 * 3
}

fn count_pixels_at_thresholds(
    buffer: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    rect: Rect,
    low_threshold: u8,
    high_threshold: u8,
    step: i32,
) -> (u32, u32) {
    if rect.is_empty() || step <= 0 {
        return (0, 0);
    }

    let bytes_per_pixel = match format {
        PixelFormat::Rgba => 4,
        PixelFormat::Bgr => 3,
    };
    let stride = width as usize * bytes_per_pixel;
    let min_len = stride.saturating_mul(height as usize);
    if buffer.len() < min_len {
        return (0, 0);
    }

    let low = low_threshold as u16 * 3;
    let high = high_threshold as u16 * 3;
    let sample_area = (step * step) as u32;
    let mut low_count = 0;
    let mut high_count = 0;

    let mut y = rect.top;
    while y < rect.bottom {
        let row = (height as i32 - 1 - y) as usize;
        let mut offset = row * stride + rect.left as usize * bytes_per_pixel;
        let mut x = rect.left;
        while x < rect.right {
            let brightness = match format {
                PixelFormat::Rgba => {
                    buffer[offset] as u16 + buffer[offset + 1] as u16 + buffer[offset + 2] as u16
                }
                PixelFormat::Bgr => {
                    buffer[offset + 2] as u16 + buffer[offset + 1] as u16 + buffer[offset] as u16
                }
            };
            if brightness >= low {
                low_count += sample_area;
                if brightness >= high {
                    high_count += sample_area;
                }
            }
            x += step;
            offset += bytes_per_pixel * step as usize;
        }
        y += step;
    }

    (low_count, high_count)
}

fn estimate_bright_pixels_sampled(
    buffer: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    rect: Rect,
    threshold: u8,
    step: i32,
) -> u32 {
    if rect.is_empty() || step <= 0 {
        return 0;
    }

    let bytes_per_pixel = match format {
        PixelFormat::Rgba => 4,
        PixelFormat::Bgr => 3,
    };
    let stride = width as usize * bytes_per_pixel;
    let min_len = stride.saturating_mul(height as usize);
    if buffer.len() < min_len {
        return 0;
    }

    let mut count = 0;
    let mut y = rect.top;
    while y < rect.bottom {
        let row = (height as i32 - 1 - y) as usize;
        let mut x = rect.left;
        while x < rect.right {
            let offset = row * stride + x as usize * bytes_per_pixel;
            let is_bright = match format {
                PixelFormat::Rgba => is_bright_enough(
                    buffer[offset],
                    buffer[offset + 1],
                    buffer[offset + 2],
                    threshold,
                ),
                PixelFormat::Bgr => is_bright_enough(
                    buffer[offset + 2],
                    buffer[offset + 1],
                    buffer[offset],
                    threshold,
                ),
            };
            if is_bright {
                count += 1;
            }
            x += step;
        }
        y += step;
    }

    count * (step as u32 * step as u32)
}

#[inline]
fn normalized_count(count: u32, scale: f64) -> f64 {
    if scale <= 0.0 {
        0.0
    } else {
        count as f64 / (scale * scale)
    }
}

#[inline]
fn classify_speed(bright: f64, dim: f64) -> SpeedButtonState {
    // A crisp 1X/2X glyph sits on a dark button (little mid-tone). Anything that
    // floods the box with mid-tones is the greyed/occluded deployment button.
    let crisp = dim - bright <= SPEED_GLYPH_CRISP_MAX;
    if crisp && (SPEED_1X_MIN..=SPEED_1X_MAX).contains(&bright) {
        SpeedButtonState::OneX
    } else if crisp && (SPEED_2X_MIN..=SPEED_2X_MAX).contains(&bright) {
        SpeedButtonState::TwoX
    } else {
        SpeedButtonState::PointTwoX
    }
}

#[inline]
fn classify_pause(bright: f64) -> Option<PauseButtonState> {
    if (PAUSE_RUNNING_MIN..=PAUSE_RUNNING_MAX).contains(&bright) {
        Some(PauseButtonState::Running)
    } else if (PAUSE_PAUSED_MIN..=PAUSE_PAUSED_MAX).contains(&bright) {
        Some(PauseButtonState::Paused)
    } else {
        None
    }
}

fn has_takeover_overlay(
    buffer: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    scale: f64,
) -> bool {
    let takeover_rect = scaled_rect(
        width,
        height,
        scale,
        TAKEOVER_OVERLAY_LEFT_REF,
        TAKEOVER_OVERLAY_RIGHT_REF,
        TAKEOVER_OVERLAY_TOP_REF,
        TAKEOVER_OVERLAY_BOTTOM_REF,
    );
    let sample_step = (scale * 2.0).round().max(2.0) as i32;
    let takeover_bright = normalized_count(
        estimate_bright_pixels_sampled(
            buffer,
            width,
            height,
            format,
            takeover_rect,
            TAKEOVER_OVERLAY_BRIGHT_THRESHOLD,
            sample_step,
        ),
        scale,
    );

    takeover_bright >= TAKEOVER_OVERLAY_BRIGHT_MIN
}

/// Classifies the top-right battle HUD into one of the [`BattleState`]s.
///
/// Two glyph boxes drive everything: the pause/play button (the reliable
/// in-battle anchor) and the speed button.
///
/// 1. If the pause box holds a valid glyph, we are in battle. Its area says
///    running (two bars) vs paused (triangle); the speed box then says
///    1x / 2x / 0.2x (a crisp bright glyph vs the greyed deployment button).
/// 2. Otherwise both glyphs are dark. Greyed-but-present button outlines with no
///    deploy overlay mean the battle is loading or settling
///    ([`BattleState::BeforeOrAfterBattle`]); anything else is not a battle.
pub fn detect_battle_state(
    buffer: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
) -> BattleState {
    if width == 0 || height == 0 {
        return BattleState::NotInBattle;
    }

    let scale = battle_button_scale(width, height);
    let count_step = if scale >= 1.5 { 2 } else { 1 };
    let speed_rect = glyph_rect_from_right(
        width,
        height,
        scale,
        SPEED_GLYPH_LEFT_FROM_RIGHT_REF,
        SPEED_GLYPH_RIGHT_FROM_RIGHT_REF,
        SPEED_GLYPH_TOP_REF,
        SPEED_GLYPH_BOTTOM_REF,
    );
    let pause_rect = glyph_rect_from_right(
        width,
        height,
        scale,
        PAUSE_GLYPH_LEFT_FROM_RIGHT_REF,
        PAUSE_GLYPH_RIGHT_FROM_RIGHT_REF,
        PAUSE_GLYPH_TOP_REF,
        PAUSE_GLYPH_BOTTOM_REF,
    );

    let (speed_dim, speed_bright) = count_pixels_at_thresholds(
        buffer,
        width,
        height,
        format,
        speed_rect,
        GLYPH_DIM_THRESHOLD,
        GLYPH_BRIGHT_THRESHOLD,
        count_step,
    );
    let (pause_dim, pause_bright) = count_pixels_at_thresholds(
        buffer,
        width,
        height,
        format,
        pause_rect,
        GLYPH_DIM_THRESHOLD,
        GLYPH_BRIGHT_THRESHOLD,
        count_step,
    );
    let speed_bright = normalized_count(speed_bright, scale);
    let speed_dim = normalized_count(speed_dim, scale);
    let pause_bright = normalized_count(pause_bright, scale);
    let pause_dim = normalized_count(pause_dim, scale);

    if let Some(pause) = classify_pause(pause_bright) {
        return match (classify_speed(speed_bright, speed_dim), pause) {
            (SpeedButtonState::PointTwoX, PauseButtonState::Running) => {
                BattleState::PointTwoXRunning
            }
            (SpeedButtonState::PointTwoX, PauseButtonState::Paused) => {
                BattleState::PointTwoXPaused
            }
            (SpeedButtonState::OneX, PauseButtonState::Running) => BattleState::OneXRunning,
            (SpeedButtonState::OneX, PauseButtonState::Paused) => BattleState::OneXPaused,
            (SpeedButtonState::TwoX, PauseButtonState::Running) => BattleState::TwoXRunning,
            (SpeedButtonState::TwoX, PauseButtonState::Paused) => BattleState::TwoXPaused,
        };
    }

    let glyphs_dark = speed_bright < GLYPH_PRESENT_MAX && pause_bright < GLYPH_PRESENT_MAX;
    let buttons_present = speed_dim >= INIT_DIM_MIN && pause_dim <= INIT_DIM_MAX;
    if glyphs_dark
        && buttons_present
        && !has_takeover_overlay(buffer, width, height, format, scale)
    {
        return BattleState::BeforeOrAfterBattle;
    }

    BattleState::NotInBattle
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

    // The leftmost ROI pixel is the most likely to catch transient HUD highlights
    // during the full→empty wrap. Ignore 1px there when deciding whether the bar
    // is still valid, but keep measuring widths against the original x1.
    let validity_x1 = (x1 + VALIDITY_LEFT_INSET_PX).min(x2 - 1);

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
        for x in (validity_x1..(x2 - 1)).rev() {
            let (r, g, b, a) = read_pixel(buffer, width, height, format, x, y_mid)?;
            if a != ALPHA_OPAQUE || !is_pixel_grayscale(r, g, b) {
                return None;
            }
            if r > WHITE_THRESHOLD && g > WHITE_THRESHOLD && b > WHITE_THRESHOLD {
                filled_width = x - x1 + 1;
                break;
            }
        }
        if filled_width == 0 && validity_x1 > x1 {
            let (r, g, b, a) = read_pixel(buffer, width, height, format, x1, y_mid)?;
            if a == ALPHA_OPAQUE
                && r > WHITE_THRESHOLD
                && g > WHITE_THRESHOLD
                && b > WHITE_THRESHOLD
            {
                filled_width = 1;
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
                for x in (validity_x1..(x2 - 1)).rev() {
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
                if filled_width == 0 && validity_x1 > x1 {
                    let (r, g, b, a) = read_pixel(buffer, width, height, format, x1, y_mid)?;
                    if a == ALPHA_OPAQUE
                        && r > MASKED_WHITE_THRESHOLD
                        && g > MASKED_WHITE_THRESHOLD
                        && b > MASKED_WHITE_THRESHOLD
                        && r <= MASKED_MAX_BRIGHTNESS
                        && g <= MASKED_MAX_BRIGHTNESS
                        && b <= MASKED_MAX_BRIGHTNESS
                    {
                        filled_width = 1;
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
    fn left_edge_tint_does_not_invalidate_empty_bar() {
        let w = 200u32;
        let h = 100u32;
        let roi = (10, 190, 50);
        let buf = make_rgba_buffer(w, h, &|x, _y| {
            if x == roi.0 as u32 {
                [67, 66, 44, 255]
            } else {
                [54, 54, 54, 255]
            }
        });

        let result = get_raw_filled_pixel_width(&buf, w, h, PixelFormat::Rgba, roi);
        assert_eq!(result, Some(0));
    }

    #[test]
    fn left_edge_white_still_counts_as_single_pixel_fill() {
        let w = 200u32;
        let h = 100u32;
        let roi = (10, 190, 50);
        let buf = make_rgba_buffer(w, h, &|x, _y| {
            if x == roi.0 as u32 {
                [252, 252, 252, 255]
            } else {
                [54, 54, 54, 255]
            }
        });

        let result = get_raw_filled_pixel_width(&buf, w, h, PixelFormat::Rgba, roi);
        assert_eq!(result, Some(1));
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

    #[test]
    fn ignores_wide_bright_non_white_run() {
        // A wide run of bright-but-not-white pixels (e.g. a reddish HUD background
        // or anti-aliased digit body) used to satisfy the loose sign test and
        // trigger a false positive. The real minus sign core is pure #ffffff.
        let width = 1280;
        let height = 720;
        let mut buf = vec![20u8; (width * height * 3) as usize];
        for y in 513..518 {
            for x in 1212..1233 {
                put_bgr_screen_pixel(&mut buf, width, height, x, y, [200, 200, 200]);
            }
        }

        assert!(!is_cost_negative(&buf, width, height, PixelFormat::Bgr));
    }

    #[test]
    fn ignores_narrow_white_run() {
        // Positive digits can clip the scan band with a short pure-white segment
        // (real samples peaked at ~10px); the minus core is ~19px at 720p. A run
        // below the minimum must not register as the sign.
        let width = 1280;
        let height = 720;
        let mut buf = vec![20u8; (width * height * 3) as usize];
        for y in 513..518 {
            for x in 1216..1226 {
                put_bgr_screen_pixel(&mut buf, width, height, x, y, [255, 255, 255]);
            }
        }

        assert!(!is_cost_negative(&buf, width, height, PixelFormat::Bgr));
    }
}
