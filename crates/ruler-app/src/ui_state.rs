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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ApiStateSnapshot {
    pub is_running: bool,
    pub current_frame: Option<i32>,
    pub total_frames_in_cycle: i32,
    pub total_elapsed_frames: i32,
    pub active_profile: Option<String>,
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
