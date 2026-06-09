/// Calibration data loading from JSON files.
/// Compatible with both old single-profile and new multi-profile formats.
use crate::analysis::mapping::CalibrationTable;
use crate::analysis::roi::find_cost_bar_roi;
use crate::analysis::synthesis::{synthesize_profiles, MIN_DETECTABLE_WIDTH};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

const MIN_INFERRED_FRAMES_PER_COST: i32 = 15;
const MAX_INFERRED_FRAMES_PER_COST: i32 = 150;

/// New multi-profile format
#[derive(Clone, Debug, Deserialize, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Serialize)]
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

        let data: CalibrationData =
            if let Ok(new_format) = serde_json::from_str::<CalibrationData>(&content) {
                if !new_format.profiles.is_empty() {
                    new_format
                } else {
                    return Err("Calibration file has empty profiles array".to_string());
                }
            } else if let Ok(old_format) = serde_json::from_str::<OldCalibrationFormat>(&content) {
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

        let tables: Vec<CalibrationTable> = data
            .profiles
            .iter()
            .map(|p| CalibrationTable::from_pixel_map(&p.pixel_map, p.total_frames))
            .collect();

        Ok(LoadedCalibration { data, tables })
    }

    /// Load from a JSON string.
    pub fn from_json(json_str: &str) -> Result<Self, String> {
        let data: CalibrationData =
            if let Ok(new_format) = serde_json::from_str::<CalibrationData>(json_str) {
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

pub fn infer_calibration_from_samples(
    cycle_samples: &[Vec<i32>],
    screen_width: u32,
    screen_height: u32,
    calibration_time: f64,
) -> Result<CalibrationData, String> {
    if cycle_samples.is_empty() {
        return Err("未能收集到任何有效的费用条循环，请保持费用条可见并重试。".to_string());
    }

    let (x1, x2, _) = find_cost_bar_roi(screen_width as i32, screen_height as i32);
    let total_bar_width = x2 - x1;
    if total_bar_width <= 0 {
        return Err("校准失败：无法根据当前分辨率定位费用条区域。".to_string());
    }

    let reliable_cycles = collect_reliable_cycle_widths(cycle_samples, total_bar_width);
    if reliable_cycles.is_empty() {
        return Err(
            "校准失败：未能收集到足够的可靠费用条宽度，请等待费用条完整变化后重试。".to_string(),
        );
    }

    let n_eff = find_fastest_matching_n(&reliable_cycles, total_bar_width).ok_or_else(|| {
        "校准失败：样本与理论费用条序列不匹配，请重新进入关卡后在正常速度下重试。".to_string()
    })?;

    let profiles = synthesize_profiles(total_bar_width, n_eff);
    if profiles.is_empty() {
        return Err("校准失败：未能构建任何有效的费用循环模型。".to_string());
    }

    Ok(CalibrationData {
        detection_mode: Some(if profiles.len() > 1 {
            "alternating".to_string()
        } else {
            "single".to_string()
        }),
        profiles,
        screen_width: Some(screen_width),
        screen_height: Some(screen_height),
        calibration_time: Some(calibration_time),
    })
}

fn collect_reliable_cycle_widths(
    cycle_samples: &[Vec<i32>],
    total_bar_width: i32,
) -> Vec<BTreeSet<i32>> {
    cycle_samples
        .iter()
        .filter_map(|sample| {
            let widths: BTreeSet<i32> = sample
                .iter()
                .copied()
                .filter(|width| *width >= MIN_DETECTABLE_WIDTH && *width < total_bar_width)
                .collect();
            (!widths.is_empty()).then_some(widths)
        })
        .collect()
}

fn find_fastest_matching_n(reliable_cycles: &[BTreeSet<i32>], total_bar_width: i32) -> Option<f64> {
    ((MIN_INFERRED_FRAMES_PER_COST * 2)..=(MAX_INFERRED_FRAMES_PER_COST * 2)).find_map(|twice_n| {
        let n_eff = twice_n as f64 / 2.0;
        let profiles = synthesize_profiles(total_bar_width, n_eff);
        profiles_fit_cycles(reliable_cycles, &profiles).then_some(n_eff)
    })
}

fn profiles_fit_cycles(reliable_cycles: &[BTreeSet<i32>], profiles: &[ProfileData]) -> bool {
    if profiles.is_empty() {
        return false;
    }

    let profile_sets = profiles
        .iter()
        .map(|profile| {
            profile
                .pixel_map
                .keys()
                .filter_map(|width| width.parse::<i32>().ok())
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();

    (0..profile_sets.len()).any(|offset| {
        reliable_cycles
            .iter()
            .enumerate()
            .all(|(cycle_index, cycle)| {
                cycle.is_subset(&profile_sets[(cycle_index + offset) % profile_sets.len()])
            })
    })
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

    #[test]
    fn infer_calibration_detects_integer_profile() {
        let samples = sample_cycles_for_n(180, 60.0);
        let data = infer_calibration_from_samples(&samples, 1920, 1080, 123.0).unwrap();
        assert_eq!(data.detection_mode, Some("single".to_string()));
        assert_eq!(data.profiles.len(), 1);
        assert_eq!(data.profiles[0].total_frames, 60);
        assert_eq!(data.screen_width, Some(1920));
        assert_eq!(data.calibration_time, Some(123.0));
    }

    #[test]
    fn infer_calibration_detects_half_frame_profile() {
        let samples = sample_cycles_for_n(180, 37.5);
        let data = infer_calibration_from_samples(&samples, 1920, 1080, 123.0).unwrap();
        assert_eq!(data.detection_mode, Some("alternating".to_string()));
        assert_eq!(data.profiles.len(), 2);
        assert_eq!(data.profiles[0].total_frames, 38);
        assert_eq!(data.profiles[1].total_frames, 37);
    }

    #[test]
    fn infer_calibration_rejects_insufficient_reliable_widths() {
        let samples = vec![vec![0, 1, 180], vec![0]];
        assert!(infer_calibration_from_samples(&samples, 1920, 1080, 123.0).is_err());
    }

    fn sample_cycles_for_n(total_bar_width: i32, n_eff: f64) -> Vec<Vec<i32>> {
        synthesize_profiles(total_bar_width, n_eff)
            .into_iter()
            .map(|profile| {
                let mut widths: Vec<i32> = profile
                    .pixel_map
                    .keys()
                    .filter_map(|width| width.parse::<i32>().ok())
                    .collect();
                widths.sort_unstable();
                widths
            })
            .collect()
    }
}
