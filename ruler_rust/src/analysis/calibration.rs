/// Calibration data loading from JSON files.
/// Compatible with both old single-profile and new multi-profile formats.

use crate::analysis::mapping::CalibrationTable;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// New multi-profile format
#[derive(Clone, Debug, Deserialize)]
pub struct CalibrationData {
    #[serde(default)]
    pub detection_mode: Option<String>,
    pub profiles: Vec<ProfileData>,
    #[serde(default)]
    pub screen_width: Option<u32>,
    #[serde(default)]
    pub screen_height: Option<u32>,
    #[serde(default)]
    pub calibration_time: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProfileData {
    pub total_frames: i32,
    pub pixel_map: HashMap<String, i32>,
}

/// Old single-profile format (for backward compatibility)
#[derive(Deserialize)]
struct OldCalibrationFormat {
    pixel_map: HashMap<String, i32>,
    total_frames: i32,
    #[serde(default)]
    screen_width: Option<u32>,
    #[serde(default)]
    screen_height: Option<u32>,
    #[serde(default)]
    calibration_time: Option<f64>,
}

pub struct LoadedCalibration {
    pub data: CalibrationData,
    /// Pre-compiled calibration tables for fast binary search
    pub tables: Vec<CalibrationTable>,
}

impl LoadedCalibration {
    /// Load calibration data from a JSON file.
    /// Supports both old single-profile and new multi-profile formats.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read calibration file: {e}"))?;

        let data: CalibrationData = if let Ok(new_format) = serde_json::from_str::<CalibrationData>(&content) {
            if !new_format.profiles.is_empty() {
                new_format
            } else {
                return Err("Calibration file has empty profiles array".to_string());
            }
        } else if let Ok(old_format) = serde_json::from_str::<OldCalibrationFormat>(&content) {
            // Convert old format to new format
            CalibrationData {
                detection_mode: Some("single".to_string()),
                profiles: vec![ProfileData {
                    total_frames: old_format.total_frames,
                    pixel_map: old_format.pixel_map,
                }],
                screen_width: old_format.screen_width,
                screen_height: old_format.screen_height,
                calibration_time: old_format.calibration_time,
            }
        } else {
            return Err("Calibration file format unrecognized".to_string());
        };

        // Build calibration tables
        let tables: Vec<CalibrationTable> = data
            .profiles
            .iter()
            .map(|p| CalibrationTable::from_pixel_map(&p.pixel_map, p.total_frames))
            .collect();

        Ok(LoadedCalibration { data, tables })
    }

    /// Load from a JSON string (for Python integration)
    pub fn from_json(json_str: &str) -> Result<Self, String> {
        let data: CalibrationData = if let Ok(new_format) = serde_json::from_str::<CalibrationData>(json_str) {
            if !new_format.profiles.is_empty() {
                new_format
            } else {
                return Err("Calibration JSON has empty profiles array".to_string());
            }
        } else if let Ok(old_format) = serde_json::from_str::<OldCalibrationFormat>(json_str) {
            CalibrationData {
                detection_mode: Some("single".to_string()),
                profiles: vec![ProfileData {
                    total_frames: old_format.total_frames,
                    pixel_map: old_format.pixel_map,
                }],
                screen_width: old_format.screen_width,
                screen_height: old_format.screen_height,
                calibration_time: old_format.calibration_time,
            }
        } else {
            return Err("Calibration JSON format unrecognized".to_string());
        };

        let tables: Vec<CalibrationTable> = data
            .profiles
            .iter()
            .map(|p| CalibrationTable::from_pixel_map(&p.pixel_map, p.total_frames))
            .collect();

        Ok(LoadedCalibration { data, tables })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_format() {
        let json = r#"{
            "detection_mode": "single",
            "profiles": [{
                "total_frames": 30,
                "pixel_map": {"0": 0, "5": 1, "10": 2, "15": 3}
            }],
            "screen_width": 1920,
            "screen_height": 1080
        }"#;
        let loaded = LoadedCalibration::from_json(json).unwrap();
        assert_eq!(loaded.tables.len(), 1);
        assert_eq!(loaded.tables[0].lookup(10), Some(2));
        assert_eq!(loaded.data.screen_width, Some(1920));
    }

    #[test]
    fn test_old_format() {
        let json = r#"{
            "total_frames": 30,
            "pixel_map": {"0": 0, "5": 1, "10": 2}
        }"#;
        let loaded = LoadedCalibration::from_json(json).unwrap();
        assert_eq!(loaded.tables.len(), 1);
        assert_eq!(loaded.data.detection_mode, Some("single".to_string()));
    }

    #[test]
    fn test_multi_profile() {
        let json = r#"{
            "detection_mode": "alternating",
            "profiles": [
                {"total_frames": 30, "pixel_map": {"0": 0, "10": 1}},
                {"total_frames": 37, "pixel_map": {"0": 0, "15": 1}}
            ]
        }"#;
        let loaded = LoadedCalibration::from_json(json).unwrap();
        assert_eq!(loaded.tables.len(), 2);
        assert_eq!(loaded.tables[0].total_frames, 30);
        assert_eq!(loaded.tables[1].total_frames, 37);
    }

    #[test]
    fn test_invalid_json() {
        let json = "not json";
        assert!(LoadedCalibration::from_json(json).is_err());
    }
}
