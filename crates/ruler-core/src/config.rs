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
}

impl RulerConfig {
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, RulerConfigError> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path)
            .map_err(|source| RulerConfigError::Io { path: path.to_path_buf(), source })?;
        serde_json::from_str(&contents).map_err(|source| RulerConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn to_capture_config(&self) -> Result<CaptureConfig, RulerConfigError> {
        let capture_type = match self.capture_type.as_str() {
            "mumu" => CaptureType::MuMu,
            "ldplayer" => CaptureType::LDPlayer,
            "window" => CaptureType::Windows,
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
            Self::UnsupportedCaptureType(value) => {
                write!(f, "unsupported capture type '{value}' in config.json")
            }
        }
    }
}

impl std::error::Error for RulerConfigError {}
