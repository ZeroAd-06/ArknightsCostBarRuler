//! Top-right battle-HUD classification into [`BattleState`].
use super::{BattleState, PixelFormat};
use crate::analysis::roi;

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

const BATTLE_BEGIN_TOP_RIGHT_DIM_MAX: f64 = 2.0;
const BATTLE_BEGIN_WHITE_MIN: u8 = 210;
const BATTLE_BEGIN_WHITE_TOLERANCE: u8 = 45;
const BATTLE_BEGIN_DARK_MAX_SUM: u16 = 150;
const BATTLE_BEGIN_SAMPLE_STEP_SCALE: f64 = 14.0;
const BATTLE_BEGIN_SIDE_AVG_MAX: u64 = 120;
const BATTLE_BEGIN_TOP_WHITE_MAX: u32 = 0;
const BATTLE_BEGIN_TOP_COLOR_DELTA_MAX: u64 = 4;
const BATTLE_BEGIN_SIDE_COLOR_DELTA_MAX: u64 = 4;
const BATTLE_BEGIN_CODE_WHITE_PERMYRIAD_MIN: u32 = 100;
const BATTLE_BEGIN_TITLE_WHITE_PERMYRIAD_MIN: u32 = 150;
const BATTLE_BEGIN_TEXT_SCORE_PERMYRIAD_MIN: u32 = 650;

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

#[derive(Clone, Copy, Debug, Default)]
struct BattleBeginBandStats {
    total: u32,
    white: u32,
    dark: u32,
    brightness_sum: u64,
    channel_delta_sum: u64,
}

impl BattleBeginBandStats {
    #[inline]
    fn white_permyriad(self) -> u32 {
        if self.total == 0 {
            0
        } else {
            self.white.saturating_mul(10_000) / self.total
        }
    }

    #[inline]
    fn avg_brightness(self) -> u64 {
        if self.total == 0 {
            u64::MAX
        } else {
            self.brightness_sum / (self.total as u64 * 3)
        }
    }

    #[inline]
    fn avg_channel_delta(self) -> u64 {
        if self.total == 0 {
            u64::MAX
        } else {
            self.channel_delta_sum / self.total as u64
        }
    }

    #[inline]
    fn is_dim_backdrop(self) -> bool {
        self.total > 0
            && self.avg_brightness() <= BATTLE_BEGIN_SIDE_AVG_MAX
            && self.avg_channel_delta() <= BATTLE_BEGIN_SIDE_COLOR_DELTA_MAX
    }
}

#[inline]
fn battle_button_scale(width: u32, height: u32, ui_scaler: f64) -> f64 {
    let aspect_ratio = width as f64 / height as f64;
    let base_scale = if aspect_ratio >= BATTLE_BUTTON_REF_ASPECT_RATIO {
        height as f64 / BATTLE_BUTTON_REF_HEIGHT
    } else {
        width as f64 / BATTLE_BUTTON_REF_WIDTH
    };
    base_scale * roi::ui_edge_scale(ui_scaler)
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
        PixelFormat::Bgra => 4,
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
                PixelFormat::Bgr | PixelFormat::Bgra => {
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
        PixelFormat::Bgra => 4,
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
                PixelFormat::Bgr | PixelFormat::Bgra => is_bright_enough(
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

#[inline(always)]
fn is_battle_begin_text_pixel(r: u8, g: u8, b: u8) -> bool {
    r >= BATTLE_BEGIN_WHITE_MIN
        && g >= BATTLE_BEGIN_WHITE_MIN
        && b >= BATTLE_BEGIN_WHITE_MIN
        && max_channel_delta(r, g, b) <= BATTLE_BEGIN_WHITE_TOLERANCE
}

#[inline(always)]
fn max_channel_delta(r: u8, g: u8, b: u8) -> u8 {
    let min = r.min(g).min(b);
    let max = r.max(g).max(b);
    max - min
}

fn sample_battle_begin_band(
    buffer: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    left_ratio: f64,
    right_ratio: f64,
    top_ratio: f64,
    bottom_ratio: f64,
    x_step: i32,
    y_step: i32,
) -> BattleBeginBandStats {
    if width == 0 || height == 0 || x_step <= 0 || y_step <= 0 {
        return BattleBeginBandStats::default();
    }

    let bytes_per_pixel = match format {
        PixelFormat::Rgba => 4,
        PixelFormat::Bgr => 3,
        PixelFormat::Bgra => 4,
    };
    let stride = width as usize * bytes_per_pixel;
    let min_len = stride.saturating_mul(height as usize);
    if buffer.len() < min_len {
        return BattleBeginBandStats::default();
    }

    let left = (width as f64 * left_ratio).round() as i32;
    let right = (width as f64 * right_ratio).round() as i32;
    let top = (height as f64 * top_ratio).round() as i32;
    let bottom = (height as f64 * bottom_ratio).round() as i32;
    let rect = Rect {
        left: left.clamp(0, width as i32),
        right: right.clamp(0, width as i32),
        top: top.clamp(0, height as i32),
        bottom: bottom.clamp(0, height as i32),
    };
    if rect.is_empty() {
        return BattleBeginBandStats::default();
    }

    let mut stats = BattleBeginBandStats::default();
    let mut y = rect.top;
    while y < rect.bottom {
        let row = (height as i32 - 1 - y) as usize;
        let mut offset = row * stride + rect.left as usize * bytes_per_pixel;
        let mut x = rect.left;
        while x < rect.right {
            let (r, g, b) = match format {
                PixelFormat::Rgba => (buffer[offset], buffer[offset + 1], buffer[offset + 2]),
                PixelFormat::Bgr | PixelFormat::Bgra => {
                    (buffer[offset + 2], buffer[offset + 1], buffer[offset])
                }
            };
            let brightness = r as u16 + g as u16 + b as u16;
            stats.total += 1;
            stats.brightness_sum += brightness as u64;
            stats.channel_delta_sum += max_channel_delta(r, g, b) as u64;
            if brightness <= BATTLE_BEGIN_DARK_MAX_SUM {
                stats.dark += 1;
            }
            if is_battle_begin_text_pixel(r, g, b) {
                stats.white += 1;
            }

            x += x_step;
            offset += bytes_per_pixel * x_step as usize;
        }
        y += y_step;
    }

    stats
}

fn has_battle_begin_title_screen(
    buffer: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    scale: f64,
) -> bool {
    let step = (scale * BATTLE_BEGIN_SAMPLE_STEP_SCALE).round().max(4.0) as i32;
    let top_y_step = ((height as f64 * 0.10).round() as i32).max(step);

    let top_clear = sample_battle_begin_band(
        buffer,
        width,
        height,
        format,
        0.20,
        0.80,
        0.03,
        0.34,
        step * 2,
        top_y_step,
    );
    if top_clear.white > BATTLE_BEGIN_TOP_WHITE_MAX
        || top_clear.avg_channel_delta() > BATTLE_BEGIN_TOP_COLOR_DELTA_MAX
    {
        return false;
    }

    let left_background = sample_battle_begin_band(
        buffer, width, height, format, 0.03, 0.18, 0.40, 0.80, step, step,
    );
    if !left_background.is_dim_backdrop() {
        return false;
    }

    let right_background = sample_battle_begin_band(
        buffer, width, height, format, 0.82, 0.97, 0.40, 0.80, step, step,
    );
    if !right_background.is_dim_backdrop() {
        return false;
    }

    let operation = sample_battle_begin_band(
        buffer, width, height, format, 0.20, 0.80, 0.38, 0.47, step, step,
    );

    let code = sample_battle_begin_band(
        buffer, width, height, format, 0.25, 0.75, 0.46, 0.58, step, step,
    );
    if code.white_permyriad() < BATTLE_BEGIN_CODE_WHITE_PERMYRIAD_MIN {
        return false;
    }

    let title = sample_battle_begin_band(
        buffer, width, height, format, 0.20, 0.80, 0.56, 0.72, step, step,
    );
    if title.white_permyriad() < BATTLE_BEGIN_TITLE_WHITE_PERMYRIAD_MIN {
        return false;
    }

    let bottom = sample_battle_begin_band(
        buffer, width, height, format, 0.20, 0.80, 0.84, 0.99, step, step,
    );
    operation
        .white_permyriad()
        .saturating_add(code.white_permyriad())
        .saturating_add(title.white_permyriad())
        .saturating_add(bottom.white_permyriad())
        >= BATTLE_BEGIN_TEXT_SCORE_PERMYRIAD_MIN
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
    detect_battle_state_with_ui_scaler(buffer, width, height, format, roi::DEFAULT_UI_SCALER)
}

pub fn detect_battle_state_with_ui_scaler(
    buffer: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    ui_scaler: f64,
) -> BattleState {
    if width == 0 || height == 0 {
        return BattleState::NotInBattle;
    }

    let scale = battle_button_scale(width, height, ui_scaler);
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
            (SpeedButtonState::PointTwoX, PauseButtonState::Paused) => BattleState::PointTwoXPaused,
            (SpeedButtonState::OneX, PauseButtonState::Running) => BattleState::OneXRunning,
            (SpeedButtonState::OneX, PauseButtonState::Paused) => BattleState::OneXPaused,
            (SpeedButtonState::TwoX, PauseButtonState::Running) => BattleState::TwoXRunning,
            (SpeedButtonState::TwoX, PauseButtonState::Paused) => BattleState::TwoXPaused,
        };
    }

    let glyphs_dark = speed_bright < GLYPH_PRESENT_MAX && pause_bright < GLYPH_PRESENT_MAX;
    let buttons_present = speed_dim >= INIT_DIM_MIN && pause_dim <= INIT_DIM_MAX;
    if glyphs_dark
        && speed_dim <= BATTLE_BEGIN_TOP_RIGHT_DIM_MAX
        && pause_dim <= BATTLE_BEGIN_TOP_RIGHT_DIM_MAX
        && has_battle_begin_title_screen(buffer, width, height, format, scale)
    {
        return BattleState::BattleBegin;
    }

    if glyphs_dark && buttons_present && !has_takeover_overlay(buffer, width, height, format, scale)
    {
        return BattleState::BeforeOrAfterBattle;
    }

    BattleState::NotInBattle
}
