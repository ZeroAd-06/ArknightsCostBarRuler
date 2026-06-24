use std::{
    fs, io,
    path::{Path, PathBuf},
};

use ruler_core::analysis::calibration::{CalibrationData, LoadedCalibration};
use serde_json::Value;

use crate::{resources::ResourceLocator, ui_state::ProfileMenuItem};

#[derive(Clone, Debug)]
pub struct ProfileStore {
    calibration_dir: PathBuf,
}

impl ProfileStore {
    #[must_use]
    pub fn new(resources: &ResourceLocator) -> Self {
        Self {
            calibration_dir: resources.calibration_dir(),
        }
    }

    #[must_use]
    pub fn calibration_path(&self, filename: &str) -> PathBuf {
        self.calibration_dir.join(filename)
    }

    #[must_use]
    pub fn list(&self, active_profile: Option<&str>) -> Vec<ProfileMenuItem> {
        let _ = fs::create_dir_all(&self.calibration_dir);
        let mut profiles = Vec::new();
        let Ok(entries) = fs::read_dir(&self.calibration_dir) else {
            return profiles;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            // Skip files that fail to load (unversioned / unsupported format) so
            // they never appear in the UI profile list.
            if LoadedCalibration::from_file(&path).is_err() {
                continue;
            }
            let Some(filename) = path
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let (total_frames_str, resolution) = profile_details(&path);
            profiles.push(ProfileMenuItem {
                basename: calibration_basename(&filename),
                is_active: active_profile == Some(filename.as_str()),
                filename,
                total_frames_str,
                resolution,
            });
        }

        profiles.sort_by(|left, right| left.filename.cmp(&right.filename));
        profiles
    }

    pub fn delete(&self, filename: &str) -> std::io::Result<()> {
        fs::remove_file(self.calibration_path(filename))
    }

    pub fn rename(&self, old: &str, new_base: &str) -> std::io::Result<String> {
        let old_path = self.calibration_path(old);
        let suffix = old
            .split_once('_')
            .map(|(_, suffix)| suffix.to_string())
            .unwrap_or_else(|| "profile.json".to_string());
        let sanitized = sanitize_profile_base(new_base);
        let new_filename = format!("{sanitized}_{suffix}");
        fs::rename(old_path, self.calibration_path(&new_filename))?;
        Ok(new_filename)
    }

    pub fn save_calibration(
        &self,
        data: &CalibrationData,
        basename: &str,
    ) -> std::io::Result<String> {
        fs::create_dir_all(&self.calibration_dir)?;
        let sanitized = sanitize_profile_base(basename);
        let frame_counts = if data.profiles.is_empty() {
            "0f".to_string()
        } else {
            format!(
                "{}f",
                data.profiles
                    .iter()
                    .map(|profile| profile.total_frames.to_string())
                    .collect::<Vec<_>>()
                    .join("-")
            )
        };
        let screen_width = data.screen_width.unwrap_or(0);
        let screen_height = data.screen_height.unwrap_or(0);
        let filename = format!("{sanitized}_{frame_counts}_{screen_width}x{screen_height}.json");
        let contents = serde_json::to_string_pretty(data).map_err(io::Error::other)?;
        fs::write(self.calibration_path(&filename), contents)?;
        Ok(filename)
    }
}

#[must_use]
pub fn calibration_basename(filename: &str) -> String {
    if let Some((base, _)) = filename.split_once('_') {
        base.to_string()
    } else {
        filename
            .strip_suffix(".json")
            .unwrap_or(filename)
            .to_string()
    }
}

fn profile_details(path: &Path) -> (String, String) {
    let Ok(contents) = fs::read_to_string(path) else {
        return ("损坏".to_string(), "未知".to_string());
    };
    let Ok(json) = serde_json::from_str::<Value>(&contents) else {
        return ("损坏".to_string(), "未知".to_string());
    };

    let total_frames_str = json
        .get("profiles")
        .and_then(Value::as_array)
        .filter(|profiles| !profiles.is_empty())
        .map(|profiles| {
            let frames = profiles
                .iter()
                .map(|profile| {
                    profile
                        .get("total_frames")
                        .and_then(Value::as_i64)
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "N/A".to_string())
                })
                .collect::<Vec<_>>()
                .join("-");
            format!("{frames}f")
        })
        .or_else(|| {
            json.get("total_frames")
                .and_then(Value::as_i64)
                .map(|value| format!("{value}f"))
        })
        .unwrap_or_else(|| "N/Af".to_string());

    let screen_width = json
        .get("screen_width")
        .and_then(Value::as_i64)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "?".to_string());
    let screen_height = json
        .get("screen_height")
        .and_then(Value::as_i64)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "?".to_string());

    (total_frames_str, format!("{screen_width}x{screen_height}"))
}

fn sanitize_profile_base(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                ch
            }
        })
        .collect();
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        "profile".to_string()
    } else {
        trimmed.to_string()
    }
}
