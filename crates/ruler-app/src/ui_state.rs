pub const FRAMES_PER_SECOND: i32 = 30;
pub const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// What triggered a timer reset — drives the overlay cover animation's label.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResetKind {
    #[default]
    Manual,
    Auto,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FrameDisplayMode {
    #[default]
    ZeroToNMinusOne,
    ZeroToN,
    OneToN,
}

impl FrameDisplayMode {
    #[must_use]
    pub fn from_config(value: Option<&str>) -> Self {
        match value {
            Some("0_to_n") => Self::ZeroToN,
            Some("1_to_n") => Self::OneToN,
            _ => Self::ZeroToNMinusOne,
        }
    }

    #[must_use]
    pub fn as_config(self) -> &'static str {
        match self {
            Self::ZeroToNMinusOne => "0_to_n-1",
            Self::ZeroToN => "0_to_n",
            Self::OneToN => "1_to_n",
        }
    }

    #[must_use]
    pub fn from_api(value: &str) -> Option<Self> {
        match value {
            "0_to_n-1" => Some(Self::ZeroToNMinusOne),
            "0_to_n" => Some(Self::ZeroToN),
            "1_to_n" => Some(Self::OneToN),
            _ => None,
        }
    }

    #[must_use]
    pub fn display_total(self, total_frames: i32) -> String {
        match self {
            Self::ZeroToNMinusOne => format!("/{}", (total_frames - 1).max(0)),
            Self::ZeroToN | Self::OneToN => format!("/{total_frames}"),
        }
    }

    #[must_use]
    pub fn display_frame(self, logical_frame: Option<i32>) -> String {
        match logical_frame {
            Some(frame) if self == Self::OneToN => (frame + 1).to_string(),
            Some(frame) => frame.to_string(),
            None => "--".to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OverlayMode {
    Booting,
    Idle,
    PreCalibration,
    Calibrating,
    Running,
    Error,
}

impl Default for OverlayMode {
    fn default() -> Self {
        Self::Booting
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProfileMenuItem {
    pub filename: String,
    pub basename: String,
    pub total_frames_str: String,
    pub resolution: String,
    pub is_active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateNotice {
    pub version: String,
    pub release_title: String,
    pub html_url: String,
    pub download_url: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ApiStateSnapshot {
    pub is_running: bool,
    pub current_frame: Option<i32>,
    pub total_frames_in_cycle: i32,
    pub total_elapsed_frames: i32,
    pub active_profile: Option<String>,
    pub frame_id: Option<u64>,
    pub sample_index: u64,
    pub dropped_since_previous: u64,
    pub raw_pixel_width: Option<i32>,
    pub cost_is_negative: bool,
    pub battle_state: Option<String>,
    pub capture_width: Option<u32>,
    pub capture_height: Option<u32>,
    pub capture_format: Option<String>,
    pub capture_timestamp_ns: Option<u64>,
    pub capture_duration_us: Option<u64>,
    pub timing_debug: Option<ruler_core::TimingDebug>,
}

impl ApiStateSnapshot {
    pub fn clear_frame_metadata(&mut self) {
        self.frame_id = None;
        self.sample_index = 0;
        self.dropped_since_previous = 0;
        self.raw_pixel_width = None;
        self.cost_is_negative = false;
        self.battle_state = None;
        self.capture_width = None;
        self.capture_height = None;
        self.capture_format = None;
        self.capture_timestamp_ns = None;
        self.capture_duration_us = None;
        self.timing_debug = None;
    }

    pub fn update_from_frame_record(&mut self, record: &ApiFrameRecord) {
        self.is_running = record.is_running;
        self.current_frame = record.current_frame;
        self.total_frames_in_cycle = if record.is_running {
            record.total_frames_in_cycle
        } else {
            0
        };
        self.total_elapsed_frames = record.total_elapsed_frames;
        self.active_profile = record.active_profile.clone();
        self.frame_id = Some(record.frame_id);
        self.sample_index = record.sample_index;
        self.dropped_since_previous = record.dropped_since_previous;
        self.raw_pixel_width = record.raw_pixel_width;
        self.cost_is_negative = record.cost_is_negative;
        self.battle_state = Some(record.battle_state.clone());
        self.capture_width = Some(record.capture_width);
        self.capture_height = Some(record.capture_height);
        self.capture_format = Some(record.capture_format.clone());
        self.capture_timestamp_ns = Some(record.capture_timestamp_ns);
        self.capture_duration_us = Some(record.capture_duration_us);
        self.timing_debug = record.timing_debug;
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ApiHistoryBounds {
    pub oldest_frame_id: Option<u64>,
    pub latest_frame_id: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiFrameRecord {
    pub frame_id: u64,
    pub sample_index: u64,
    pub dropped_since_previous: u64,
    pub is_running: bool,
    pub current_frame: Option<i32>,
    pub total_frames_in_cycle: i32,
    pub total_elapsed_frames: i32,
    pub active_profile: Option<String>,
    pub raw_pixel_width: Option<i32>,
    pub cost_is_negative: bool,
    pub battle_state: String,
    pub capture_width: u32,
    pub capture_height: u32,
    pub capture_format: String,
    pub capture_timestamp_ns: u64,
    pub capture_duration_us: u64,
    pub timing_debug: Option<ruler_core::TimingDebug>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApiFrameLookup {
    Found {
        requested_frame_id: u64,
        record: ApiFrameRecord,
        fell_back: bool,
        fallback_reason: Option<String>,
    },
    NotRetained {
        requested_frame_id: u64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiSnapshot {
    pub mode: OverlayMode,
    pub message: String,
    pub progress_percent: f32,
    pub display_frame: String,
    pub display_total: String,
    pub time_str: String,
    pub lap_frames: Option<i32>,
    pub can_undo_reset: bool,
    pub total_frames_in_cycle: i32,
    pub active_profile: Option<String>,
    pub display_mode: FrameDisplayMode,
    pub profiles: Vec<ProfileMenuItem>,
    pub capture_dimensions: Option<(u32, u32)>,
    pub overlay_scale_pct: u16,
    pub should_exit: bool,
    pub cursor_blocked: bool,
    pub update_notice: Option<UpdateNotice>,
    // Monotonic counter bumped each time the timer is reset (manual or auto).
    // The overlay compares this against its own copy to detect new resets and
    // drive the cover animation. Wrapping is fine — only inequality matters.
    pub reset_pulse: u32,
    pub reset_kind: ResetKind,
}

impl Default for UiSnapshot {
    fn default() -> Self {
        Self {
            mode: OverlayMode::Booting,
            message: "booting".to_string(),
            progress_percent: 0.0,
            display_frame: "--".to_string(),
            display_total: "/--".to_string(),
            time_str: "00:00:00".to_string(),
            lap_frames: None,
            can_undo_reset: false,
            total_frames_in_cycle: 0,
            active_profile: None,
            display_mode: FrameDisplayMode::ZeroToNMinusOne,
            profiles: Vec::new(),
            capture_dimensions: None,
            overlay_scale_pct: 100,
            should_exit: false,
            cursor_blocked: false,
            update_notice: None,
            reset_pulse: 0,
            reset_kind: ResetKind::Manual,
        }
    }
}

#[must_use]
pub fn format_time_from_frames(total_frames: i32) -> String {
    if total_frames < 0 {
        return "00:00:00".to_string();
    }

    let frames = total_frames % FRAMES_PER_SECOND;
    let total_seconds = total_frames / FRAMES_PER_SECOND;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}:{frames:02}")
}

#[must_use]
pub fn parse_timer_input_frames(input: &str) -> Option<i32> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    if !trimmed.contains(':') {
        return parse_timer_number(trimmed);
    }

    let mut parts = trimmed.split(':');
    let (Some(minutes), Some(seconds), Some(frames), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };

    let minutes = parse_timer_number(minutes)?;
    let seconds = parse_timer_number(seconds)?;
    let frames = parse_timer_number(frames)?;
    if seconds >= 60 || frames >= FRAMES_PER_SECOND {
        return None;
    }

    minutes
        .checked_mul(60)?
        .checked_add(seconds)?
        .checked_mul(FRAMES_PER_SECOND)?
        .checked_add(frames)
}

fn parse_timer_number(segment: &str) -> Option<i32> {
    if segment.is_empty() || !segment.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    segment.parse::<i32>().ok()
}

#[cfg(test)]
mod tests {
    use super::parse_timer_input_frames;

    #[test]
    fn parse_timer_input_frames_accepts_timecode_when_minutes_seconds_and_frames() {
        assert_eq!(parse_timer_input_frames("01:02:03"), Some(1_863));
        assert_eq!(parse_timer_input_frames("1:2:3"), Some(1_863));
    }

    #[test]
    fn parse_timer_input_frames_accepts_bare_frames_when_single_number() {
        assert_eq!(parse_timer_input_frames("42"), Some(42));
        assert_eq!(parse_timer_input_frames(" 0007 "), Some(7));
    }

    #[test]
    fn parse_timer_input_frames_rejects_invalid_or_out_of_range_timecodes() {
        assert_eq!(parse_timer_input_frames(""), None);
        assert_eq!(parse_timer_input_frames("1:2"), None);
        assert_eq!(parse_timer_input_frames("1:60:0"), None);
        assert_eq!(parse_timer_input_frames("1:0:30"), None);
        assert_eq!(parse_timer_input_frames("-1"), None);
    }
}
