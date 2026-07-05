use std::{
    fs, io,
    path::{Path, PathBuf},
};

use ruler_core::analysis::calibration::CalibrationData;

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
            if CalibrationData::from_file(&path).is_err() {
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
        let frame_counts = format!("{}f", frames_label(data.n_eff()));
        let filename = format!("{sanitized}_{frame_counts}.json");
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
    let Ok(data) = CalibrationData::from_file(path) else {
        return ("损坏".to_string(), "未知".to_string());
    };
    (
        format!("{}f", frames_label(data.n_eff())),
        "自适应".to_string(),
    )
}

/// Human-readable frames-per-cost label (e.g. `30`, `37.5`, `27.27`) for
/// calibration filenames and the profile menu. Trims trailing zeros.
fn frames_label(n_eff: f64) -> String {
    if !n_eff.is_finite() || n_eff <= 0.0 {
        return "0".to_string();
    }
    if (n_eff - n_eff.round()).abs() < 1e-6 {
        format!("{}", n_eff.round() as i64)
    } else {
        let s = format!("{n_eff:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
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
