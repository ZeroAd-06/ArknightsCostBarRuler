//! Zero-copy pixel scanner for cost-bar and battle-state analysis.
//! Operates directly on raw RGBA/BGR/BGRA buffers.

mod battle_state;
mod cost_bar;

pub use battle_state::{detect_battle_state, detect_battle_state_with_ui_scaler};
pub use cost_bar::{get_raw_filled_pixel_width, is_cost_negative, is_cost_negative_with_ui_scaler};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    Rgba,
    Bgr,
    /// BGRA8 (B,G,R,A byte order). Produced by the Windows Graphics Capture
    /// backend. Same channel order as `Bgr` with a trailing alpha byte.
    Bgra,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BattleState {
    PointTwoXRunning,
    OneXRunning,
    TwoXRunning,
    PointTwoXPaused,
    OneXPaused,
    TwoXPaused,
    BattleBegin,
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
            Self::BattleBegin => "battle_begin",
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
