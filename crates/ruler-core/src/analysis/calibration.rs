/// Calibration data loading from JSON files.
/// Compatible with both old single-profile and new multi-profile formats.
use crate::analysis::mapping::CalibrationTable;
use crate::analysis::roi::DEFAULT_UI_SCALER;
use crate::analysis::synthesis::{synthesize_profiles, MIN_DETECTABLE_WIDTH};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

const MIN_INFERRED_FRAMES_PER_COST: i32 = 15;
const MAX_INFERRED_FRAMES_PER_COST: i32 = 150;
const MIN_RELIABLE_WIDTHS_FOR_INFERENCE: usize = 4;
const INFERENCE_DENOMINATORS: &[i32] = &[1, 2, 11];

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
    pub ui_scaler: Option<f64>,
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
    ui_scaler: Option<f64>,
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
                    ui_scaler: old_format.ui_scaler,
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
                    ui_scaler: old_format.ui_scaler,
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
    infer_calibration_from_samples_with_ui_scaler(
        cycle_samples,
        screen_width,
        screen_height,
        DEFAULT_UI_SCALER,
        calibration_time,
    )
}

pub fn infer_calibration_from_samples_with_ui_scaler(
    cycle_samples: &[Vec<i32>],
    screen_width: u32,
    screen_height: u32,
    ui_scaler: f64,
    calibration_time: f64,
) -> Result<CalibrationData, String> {
    if cycle_samples.is_empty() {
        return Err("未能收集到任何有效的费用条循环，请保持费用条可见并重试。".to_string());
    }

    let observed_max_width = observed_max_raw_width(cycle_samples).ok_or_else(|| {
        "校准失败：未能从样本中确定费用条宽度，请保持费用条可见并重试。".to_string()
    })?;
    let (total_bar_width, n_eff, profile_offset) =
        find_matching_model(cycle_samples, observed_max_width).ok_or_else(|| {
            "校准失败：样本与理论费用条序列不匹配，请重新进入关卡后在正常速度下重试。".to_string()
        })?;

    let mut profiles = synthesize_profiles(total_bar_width, n_eff);
    if profiles.is_empty() {
        return Err("校准失败：未能构建任何有效的费用循环模型。".to_string());
    }
    let profile_count = profiles.len();
    profiles.rotate_left(profile_offset % profile_count);

    Ok(CalibrationData {
        detection_mode: Some(if profiles.len() > 1 {
            "alternating".to_string()
        } else {
            "single".to_string()
        }),
        profiles,
        screen_width: Some(screen_width),
        screen_height: Some(screen_height),
        ui_scaler: Some(ui_scaler),
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

fn observed_max_raw_width(cycle_samples: &[Vec<i32>]) -> Option<i32> {
    cycle_samples
        .iter()
        .flat_map(|sample| sample.iter().copied())
        .filter(|width| *width >= MIN_DETECTABLE_WIDTH)
        .max()
}

fn find_matching_model(
    cycle_samples: &[Vec<i32>],
    observed_max_width: i32,
) -> Option<(i32, f64, usize)> {
    let candidates = total_bar_width_candidates(observed_max_width)
        .into_iter()
        .filter_map(|total_bar_width| {
            let reliable_cycles = collect_reliable_cycle_widths(cycle_samples, total_bar_width);
            let reliable_width_count = reliable_width_count(&reliable_cycles);
            (reliable_width_count >= MIN_RELIABLE_WIDTHS_FOR_INFERENCE).then_some((
                total_bar_width,
                reliable_cycles,
                reliable_width_count,
            ))
        })
        .collect::<Vec<_>>();

    for n_eff in inference_candidates() {
        let mut best_for_n: Option<(usize, i32, usize)> = None;

        for (total_bar_width, reliable_cycles, reliable_width_count) in &candidates {
            let profiles = synthesize_profiles(*total_bar_width, n_eff);
            if let Some(offset) = matching_profile_offset(reliable_cycles, &profiles) {
                let should_replace = best_for_n.as_ref().map_or(
                    true,
                    |(best_width_count, best_total_bar_width, _)| {
                        *reliable_width_count > *best_width_count
                            || (*reliable_width_count == *best_width_count
                                && *total_bar_width < *best_total_bar_width)
                    },
                );
                if should_replace {
                    best_for_n = Some((*reliable_width_count, *total_bar_width, offset));
                }
            }
        }

        if let Some((_, total_bar_width, offset)) = best_for_n {
            return Some((total_bar_width, n_eff, offset));
        }
    }

    None
}

fn total_bar_width_candidates(observed_max_width: i32) -> Vec<i32> {
    let slack = ((observed_max_width as f64) * 0.08).ceil() as i32;
    let slack = slack.max(16);
    (observed_max_width..=observed_max_width.saturating_add(slack)).collect()
}

fn reliable_width_count(reliable_cycles: &[BTreeSet<i32>]) -> usize {
    reliable_cycles
        .iter()
        .flat_map(|cycle| cycle.iter().copied())
        .collect::<BTreeSet<_>>()
        .len()
}

fn inference_candidates() -> Vec<f64> {
    let mut candidates = Vec::new();
    for denominator in INFERENCE_DENOMINATORS {
        let min_numerator = MIN_INFERRED_FRAMES_PER_COST * denominator;
        let max_numerator = MAX_INFERRED_FRAMES_PER_COST * denominator;
        for numerator in min_numerator..=max_numerator {
            candidates.push(numerator as f64 / *denominator as f64);
        }
    }
    candidates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    candidates.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    candidates
}

fn matching_profile_offset(
    reliable_cycles: &[BTreeSet<i32>],
    profiles: &[ProfileData],
) -> Option<usize> {
    if profiles.is_empty() {
        return None;
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

    (0..profile_sets.len()).find(|offset| {
        reliable_cycles
            .iter()
            .enumerate()
            .all(|(cycle_index, cycle)| {
                cycle.is_subset(&profile_sets[(cycle_index + *offset) % profile_sets.len()])
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
    fn infer_calibration_uses_observed_raw_width_as_total_width() {
        let samples = sample_cycles_for_n(157, 30.0);
        let data = infer_calibration_from_samples_with_ui_scaler(&samples, 1920, 1080, 0.0, 123.0)
            .unwrap();

        assert!(data.profiles[0].pixel_map.contains_key("157"));
        assert!(!data.profiles[0].pixel_map.contains_key("180"));
        assert_eq!(data.ui_scaler, Some(0.0));
    }

    #[test]
    fn infer_calibration_recovers_when_full_width_frame_is_missing() {
        let mut samples = sample_cycles_for_n(180, 30.0);
        for sample in &mut samples {
            sample.retain(|width| *width < 180);
        }

        let data = infer_calibration_from_samples(&samples, 1920, 1080, 123.0).unwrap();

        assert!(data.profiles[0].pixel_map.contains_key("180"));
        assert_eq!(data.profiles[0].total_frames, 30);
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
    fn infer_calibration_detects_tenth_speed_bonus_profile() {
        let samples = sample_cycles_for_n(120, 30.0 / 1.1);
        let data = infer_calibration_from_samples(&samples, 1280, 720, 123.0).unwrap();

        assert_eq!(data.detection_mode, Some("alternating".to_string()));
        assert_eq!(data.profiles.len(), 11);
        assert_eq!(
            data.profiles
                .iter()
                .map(|profile| profile.total_frames)
                .sum::<i32>(),
            300
        );
    }

    #[test]
    fn infer_calibration_rotates_profiles_to_observed_offset() {
        let expected = synthesize_profiles(180, 37.5);
        let mut samples = sample_cycles_for_n(180, 37.5);
        samples.rotate_left(1);

        let data = infer_calibration_from_samples(&samples, 1920, 1080, 123.0).unwrap();

        assert_eq!(data.profiles[0].pixel_map, expected[1].pixel_map);
        assert_eq!(data.profiles[1].pixel_map, expected[0].pixel_map);
    }

    #[test]
    fn infer_calibration_detects_ce5_720p_90f_profile() {
        let samples = vec![vec![
            0, 2, 3, 5, 6, 7, 9, 10, 12, 13, 14, 16, 17, 18, 20, 21, 23, 24, 25, 27, 28, 30, 31,
            32, 34, 35, 37, 38, 39, 41, 42, 43, 45, 46, 48, 49, 50, 52, 53, 55, 56, 57, 59, 60, 61,
            63, 64, 66, 67, 68, 70, 71, 73, 74, 75, 77, 78, 79, 81, 82, 84, 85, 86, 88, 89, 91, 92,
            93, 95, 96, 97, 99, 100, 102, 103, 104, 106, 107, 109, 110, 111, 113, 114, 115, 117,
            118, 120,
        ]];

        let data = infer_calibration_from_samples(&samples, 1280, 720, 123.0).unwrap();

        assert_eq!(data.detection_mode, Some("single".to_string()));
        assert_eq!(data.profiles.len(), 1);
        assert_eq!(data.profiles[0].total_frames, 90);
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
