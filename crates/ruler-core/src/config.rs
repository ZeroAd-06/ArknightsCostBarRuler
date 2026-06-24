use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::capture::{CaptureConfig, CaptureType};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RulerConfig {
    #[serde(rename = "type")]
    pub capture_type: String,
    #[serde(default)]
    pub install_path: Option<String>,
    #[serde(default)]
    pub instance_index: Option<u32>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub window_handle: Option<isize>,
    #[serde(default)]
    pub window_title: Option<String>,
    #[serde(default)]
    pub window_class: Option<String>,
    #[serde(default)]
    pub active_calibration_profile: Option<String>,
    #[serde(default)]
    pub frame_display_mode: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub auto_select_target: bool,
    #[serde(default)]
    pub target_fingerprint: Option<String>,
    /// Overlay window top-left in screen pixels (persisted across runs).
    #[serde(default)]
    pub overlay_pos_x: Option<i32>,
    #[serde(default)]
    pub overlay_pos_y: Option<i32>,
    /// Overlay scale multiplier (1.0 == 100%, the height-aligned default).
    #[serde(default)]
    pub overlay_scale: Option<f32>,
    /// Arknights PC "UI比例缩放" value. 1.0 matches emulator layout, 0.0 uses
    /// 90% edge length.
    #[serde(default)]
    pub ui_scaler: Option<f64>,

    // -- Debug recording / logging ------------------------------------------
    /// Master switch: enable background recording of capture + analysis data.
    #[serde(default)]
    pub debug_recording_enabled: bool,

    /// Record lossless HEVC video when debug_recording_enabled.
    #[serde(default)]
    pub debug_recording_video: bool,

    /// Record analysis CSV when debug_recording_enabled.
    #[serde(default)]
    pub debug_recording_csv: bool,

    /// Enable extra-high-volume trace logging in the app/core pipelines.
    #[serde(default)]
    pub trace_logging_enabled: bool,

    /// Output root directory for logs and debug artifacts.
    /// Relative paths are resolved under the app's configured data root;
    /// defaults to "log" when not set.
    #[serde(default)]
    pub log_output_dir: Option<String>,

    // -- Replay (virtual capture from a recorded video file) ---------------
    /// Path to a pre-recorded video file for replay (type: "replay").
    /// Any ffmpeg-supported container video file is accepted.
    #[serde(default)]
    pub replay_video_path: Option<String>,

    /// Fallback playback frame rate for replay when the input file does not
    /// expose usable per-frame timestamps (default 60).
    #[serde(default)]
    pub replay_fps: Option<f64>,
}

impl RulerConfig {
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, RulerConfigError> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path).map_err(|source| RulerConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&contents).map_err(|source| RulerConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<(), RulerConfigError> {
        let path = path.as_ref();
        let contents =
            serde_json::to_string_pretty(self).map_err(|source| RulerConfigError::Serialize {
                path: path.to_path_buf(),
                source,
            })?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|source| RulerConfigError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        }
        fs::write(path, contents).map_err(|source| RulerConfigError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn normalized_frame_display_mode(&self) -> &str {
        self.frame_display_mode.as_deref().unwrap_or("0_to_n-1")
    }

    pub fn effective_ui_scaler(&self) -> f64 {
        self.ui_scaler
            .filter(|value| value.is_finite())
            .unwrap_or(crate::analysis::roi::DEFAULT_UI_SCALER)
            .clamp(0.0, 1.0)
    }

    /// The UI scaler to apply to cost-bar geometry.
    ///
    /// `ui_scaler` mirrors the Arknights **PC client** `uiScaler` setting and
    /// is meaningless for emulator/ADB capture, whose layout always matches the
    /// 1920×1080 reference (scaler = 1.0). A stale PC value left in the config
    /// must therefore be ignored for non-PC capture — otherwise it shifts the
    /// cost-bar ROI off the actual bar. Returns the configured value only for
    /// `window` (PC) capture, and `DEFAULT_UI_SCALER` for everything else.
    pub fn resolved_ui_scaler(&self) -> f64 {
        if self.capture_type == "window" {
            self.effective_ui_scaler()
        } else {
            crate::analysis::roi::DEFAULT_UI_SCALER
        }
    }

    pub fn to_capture_config(&self) -> Result<CaptureConfig, RulerConfigError> {
        let capture_type = match self.capture_type.as_str() {
            "adb" | "minicap" => CaptureType::Adb,
            "mumu" => CaptureType::MuMu,
            "ldplayer" => CaptureType::LDPlayer,
            "window" => CaptureType::Windows,
            "replay" => CaptureType::Replay,
            other => return Err(RulerConfigError::UnsupportedCaptureType(other.to_string())),
        };

        Ok(CaptureConfig {
            capture_type,
            install_path: self.install_path.clone(),
            instance_index: self.instance_index.unwrap_or(0),
            device_id: self.device_id.clone(),
            window_handle: self.window_handle,
            window_title: self.window_title.clone(),
            window_class: self.window_class.clone(),
            replay_video_path: self.replay_video_path.clone(),
            replay_fps: self.replay_fps,
        })
    }
}

#[derive(Debug)]
pub enum RulerConfigError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    Serialize {
        path: PathBuf,
        source: serde_json::Error,
    },
    UnsupportedCaptureType(String),
}

impl fmt::Display for RulerConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "failed to read config '{}': {source}", path.display())
            }
            Self::Parse { path, source } => {
                write!(f, "failed to parse config '{}': {source}", path.display())
            }
            Self::Serialize { path, source } => {
                write!(
                    f,
                    "failed to serialize config '{}': {source}",
                    path.display()
                )
            }
            Self::UnsupportedCaptureType(value) => {
                write!(f, "unsupported capture type '{value}' in config.json")
            }
        }
    }
}

impl std::error::Error for RulerConfigError {}

#[cfg(test)]
mod tests {
    use super::RulerConfig;
    use std::{fs, path::PathBuf, time::SystemTime};

    #[test]
    fn save_to_path_creates_parent_directories() {
        let root = unique_temp_dir("ruler_config_save");
        let path = root.join("nested").join("config.json");
        let config = minimal_config();

        config.save_to_path(&path).unwrap();

        assert!(path.is_file());
        let loaded = RulerConfig::load_from_path(&path).unwrap();
        assert_eq!(loaded.capture_type, "replay");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn save_writes_new_log_output_dir_field_name() {
        let root = unique_temp_dir("ruler_config_log_field");
        let path = root.join("config.json");
        let mut config = minimal_config();
        config.log_output_dir = Some("log".to_string());

        config.save_to_path(&path).unwrap();

        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("\"log_output_dir\""));
        assert!(!saved.contains("debug_recording_output_dir"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolved_ui_scaler_is_pc_only() {
        use crate::analysis::roi::DEFAULT_UI_SCALER;

        // PC (window) capture honours the configured uiScaler, including 0.0.
        let mut pc = minimal_config();
        pc.capture_type = "window".to_string();
        pc.ui_scaler = Some(0.0);
        assert_eq!(pc.resolved_ui_scaler(), 0.0);

        // Emulator/ADB/replay capture ignores a stale PC uiScaler and falls
        // back to the reference layout (1.0).
        for capture in ["mumu", "adb", "ldplayer", "replay"] {
            let mut emu = minimal_config();
            emu.capture_type = capture.to_string();
            emu.ui_scaler = Some(0.0);
            assert_eq!(
                emu.resolved_ui_scaler(),
                DEFAULT_UI_SCALER,
                "capture_type={capture} must ignore the PC-only uiScaler"
            );
        }

        // PC capture with no configured value falls back to the default.
        let mut pc_default = minimal_config();
        pc_default.capture_type = "window".to_string();
        pc_default.ui_scaler = None;
        assert_eq!(pc_default.resolved_ui_scaler(), DEFAULT_UI_SCALER);
    }

    fn minimal_config() -> RulerConfig {
        RulerConfig {
            capture_type: "replay".to_string(),
            install_path: None,
            instance_index: None,
            device_id: None,
            window_handle: None,
            window_title: None,
            window_class: None,
            active_calibration_profile: None,
            frame_display_mode: None,
            language: None,
            auto_select_target: false,
            target_fingerprint: None,
            overlay_pos_x: None,
            overlay_pos_y: None,
            overlay_scale: None,
            ui_scaler: None,
            debug_recording_enabled: false,
            debug_recording_video: false,
            debug_recording_csv: false,
            trace_logging_enabled: false,
            log_output_dir: None,
            replay_video_path: None,
            replay_fps: None,
        }
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{nanos}"))
    }
}
