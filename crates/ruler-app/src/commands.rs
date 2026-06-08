use crate::ui_state::FrameDisplayMode;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiCommand {
    PrepareCalibration,
    StartCalibration,
    UseProfile {
        filename: String,
    },
    #[allow(dead_code)]
    RenameProfile {
        old: String,
        new_base: String,
    },
    DeleteProfile {
        filename: String,
    },
    SetDisplayMode(FrameDisplayMode),
    AdjustTimer {
        frames: i32,
    },
    ResetTimer,
    ToggleLapTimer,
    Exit,
}
