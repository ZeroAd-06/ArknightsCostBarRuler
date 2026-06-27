/// Calibration data loading from JSON files (versioned multi-profile format).
use crate::analysis::mapping::CalibrationTable;
use crate::analysis::roi::{
    cost_bar_width_frac_with_ui_scaler, find_cost_bar_roi_with_ui_scaler, DEFAULT_UI_SCALER,
};
use crate::analysis::synthesis::{synthesize_profiles, MIN_DETECTABLE_WIDTH};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

const MIN_INFERRED_FRAMES_PER_COST: i32 = 15;
const MAX_INFERRED_FRAMES_PER_COST: i32 = 150;
const MIN_RELIABLE_WIDTHS_FOR_INFERENCE: usize = 4;
const INFERENCE_DENOMINATORS: &[i32] = &[1, 2, 11];
pub const TIMING_MODEL_OPEN_INTERIOR_V1: &str = "open_interior_v1";
pub const DEFAULT_BOUNDARY_SWITCH_FRAME: i32 = 315;
/// Current on-disk calibration schema version. Files without a matching
/// `format_version` are rejected (and silently dropped from the UI list).
pub const CALIBRATION_FORMAT_VERSION: u32 = 2;

/// Versioned multi-profile calibration format.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CalibrationData {
    #[serde(default)]
    pub format_version: u32,
    #[serde(default)]
    pub detection_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bar_width: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary_switch_frame: Option<i32>,
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

pub struct LoadedCalibration {
    pub data: CalibrationData,
    /// Pre-compiled calibration tables for fast binary search
    pub tables: Vec<CalibrationTable>,
    pub timing_model: CalibrationTimingModel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalibrationTimingModel {
    OpenInteriorV1 {
        total_bar_width: i32,
        boundary_switch_frame: i32,
    },
}

impl LoadedCalibration {
    /// Load calibration data from a JSON file.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read calibration file: {e}"))?;
        Self::from_json(&content)
    }

    /// Load and compile calibration from a JSON string.
    pub fn from_json(json_str: &str) -> Result<Self, String> {
        let data: CalibrationData = serde_json::from_str(json_str)
            .map_err(|e| format!("Calibration JSON parse error: {e}"))?;

        if data.format_version != CALIBRATION_FORMAT_VERSION {
            return Err(format!(
                "Unsupported calibration format_version {} (expected {CALIBRATION_FORMAT_VERSION})",
                data.format_version
            ));
        }
        if data.profiles.is_empty() {
            return Err("Calibration has empty profiles array".to_string());
        }

        let timing_model = compile_timing_model(&data)?;
        let tables: Vec<CalibrationTable> = data
            .profiles
            .iter()
            .map(|p| CalibrationTable::from_pixel_map(&p.pixel_map, p.total_frames))
            .collect();

        Ok(LoadedCalibration {
            data,
            tables,
            timing_model,
        })
    }
}

fn compile_timing_model(data: &CalibrationData) -> Result<CalibrationTimingModel, String> {
    match data.timing_model.as_deref() {
        Some(TIMING_MODEL_OPEN_INTERIOR_V1) => {
            let total_bar_width = data.total_bar_width.ok_or_else(|| {
                "open_interior_v1 calibration requires total_bar_width".to_string()
            })?;
            if total_bar_width <= 0 {
                return Err("open_interior_v1 calibration has invalid total_bar_width".to_string());
            }
            Ok(CalibrationTimingModel::OpenInteriorV1 {
                total_bar_width,
                boundary_switch_frame: data
                    .boundary_switch_frame
                    .unwrap_or(DEFAULT_BOUNDARY_SWITCH_FRAME)
                    .max(0),
            })
        }
        None => Err("calibration requires a timing_model".to_string()),
        Some(other) => Err(format!("Unsupported calibration timing_model: {other}")),
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
    let total_bar_width = total_bar_width_from_screen(screen_width, screen_height, ui_scaler);
    let bar_width_frac =
        cost_bar_width_frac_with_ui_scaler(screen_width as i32, screen_height as i32, ui_scaler);
    infer_calibration_from_samples_with_ui_scaler_and_total_bar_width(
        cycle_samples,
        screen_width,
        screen_height,
        ui_scaler,
        total_bar_width,
        Some(bar_width_frac),
        calibration_time,
    )
}

pub fn infer_calibration_from_samples_with_ui_scaler_and_total_bar_width(
    cycle_samples: &[Vec<i32>],
    screen_width: u32,
    screen_height: u32,
    ui_scaler: f64,
    total_bar_width: i32,
    bar_width_frac: Option<f64>,
    calibration_time: f64,
) -> Result<CalibrationData, String> {
    if cycle_samples.is_empty() {
        return Err("未能收集到任何有效的费用条循环，请保持费用条可见并重试。".to_string());
    }
    if total_bar_width <= 0 {
        return Err("校准失败：费用条 ROI 宽度无效，请重新配置截图区域。".to_string());
    }

    // Bar length L is built from the sub-pixel width (x2 - x1) when the caller
    // can supply it; otherwise fall back to the integer width. See
    // roi::cost_bar_width_frac_with_ui_scaler for why this matters.
    let bar_width_frac = bar_width_frac
        .filter(|frac| frac.is_finite() && *frac > 0.0)
        .unwrap_or(total_bar_width as f64);

    let observed_max_width = observed_max_raw_width(cycle_samples).ok_or_else(|| {
        "校准失败：未能从样本中确定费用条宽度，请保持费用条可见并重试。".to_string()
    })?;
    let (n_eff, profile_offset) = find_matching_model(
        cycle_samples,
        total_bar_width,
        bar_width_frac,
        observed_max_width,
    )
    .ok_or_else(|| {
        "校准失败：样本与理论费用条序列不匹配，请重新进入关卡后在正常速度下重试。".to_string()
    })?;

    let mut profiles = synthesize_profiles(total_bar_width, bar_width_frac, n_eff);
    if profiles.is_empty() {
        return Err("校准失败：未能构建任何有效的费用循环模型。".to_string());
    }
    let profile_count = profiles.len();
    profiles.rotate_left(profile_offset % profile_count);

    Ok(CalibrationData {
        format_version: CALIBRATION_FORMAT_VERSION,
        detection_mode: Some(if profiles.len() > 1 {
            "alternating".to_string()
        } else {
            "single".to_string()
        }),
        timing_model: Some(TIMING_MODEL_OPEN_INTERIOR_V1.to_string()),
        total_bar_width: Some(total_bar_width),
        boundary_switch_frame: Some(DEFAULT_BOUNDARY_SWITCH_FRAME),
        profiles,
        screen_width: Some(screen_width),
        screen_height: Some(screen_height),
        ui_scaler: Some(ui_scaler),
        calibration_time: Some(calibration_time),
    })
}

fn total_bar_width_from_screen(screen_width: u32, screen_height: u32, ui_scaler: f64) -> i32 {
    let (x1, x2, _) =
        find_cost_bar_roi_with_ui_scaler(screen_width as i32, screen_height as i32, ui_scaler);
    x2 - x1
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
    total_bar_width: i32,
    bar_width_frac: f64,
    observed_max_width: i32,
) -> Option<(f64, usize)> {
    let reliable_cycles = collect_reliable_cycle_widths(cycle_samples, total_bar_width);
    let reliable_width_count = reliable_width_count(&reliable_cycles);
    if reliable_width_count < MIN_RELIABLE_WIDTHS_FOR_INFERENCE {
        return None;
    }

    let mut best_match: Option<ModelMatch> = None;
    for n_eff in inference_candidates() {
        let profiles = synthesize_profiles(total_bar_width, bar_width_frac, n_eff);
        if let Some((offset, extra_width_count)) = matching_profile_offset_and_extra_width_count(
            &reliable_cycles,
            &profiles,
            total_bar_width,
        ) {
            let candidate = ModelMatch {
                reliable_width_count,
                extra_width_count,
                edge_error: synthesized_edge_error(&profiles, total_bar_width, observed_max_width),
                total_bar_width,
                n_eff,
                offset,
            };
            if best_match
                .as_ref()
                .map_or(true, |best| candidate.is_better_than(best))
            {
                best_match = Some(candidate);
            }
        }
    }

    best_match.map(|matched| (matched.n_eff, matched.offset))
}

#[derive(Clone, Copy, Debug)]
struct ModelMatch {
    reliable_width_count: usize,
    extra_width_count: usize,
    edge_error: i32,
    total_bar_width: i32,
    n_eff: f64,
    offset: usize,
}

impl ModelMatch {
    fn is_better_than(&self, other: &Self) -> bool {
        self.reliable_width_count > other.reliable_width_count
            || (self.reliable_width_count == other.reliable_width_count
                && self.n_eff < other.n_eff - 1e-9)
            || (self.reliable_width_count == other.reliable_width_count
                && (self.n_eff - other.n_eff).abs() < 1e-9
                && self.extra_width_count < other.extra_width_count)
            || (self.reliable_width_count == other.reliable_width_count
                && (self.n_eff - other.n_eff).abs() < 1e-9
                && self.extra_width_count == other.extra_width_count
                && self.edge_error < other.edge_error)
            || (self.reliable_width_count == other.reliable_width_count
                && (self.n_eff - other.n_eff).abs() < 1e-9
                && self.extra_width_count == other.extra_width_count
                && self.edge_error == other.edge_error
                && self.total_bar_width < other.total_bar_width)
    }
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

fn matching_profile_offset_and_extra_width_count(
    reliable_cycles: &[BTreeSet<i32>],
    profiles: &[ProfileData],
    total_bar_width: i32,
) -> Option<(usize, usize)> {
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
                .filter(|width| *width >= MIN_DETECTABLE_WIDTH && *width < total_bar_width)
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();

    (0..profile_sets.len())
        .filter_map(|offset| {
            let mut extra_width_count = 0usize;
            for (cycle_index, cycle) in reliable_cycles.iter().enumerate() {
                let profile_set = &profile_sets[(cycle_index + offset) % profile_sets.len()];
                if !cycle.is_subset(profile_set) {
                    return None;
                }
                extra_width_count += profile_set.difference(cycle).count();
            }
            Some((offset, extra_width_count))
        })
        .min_by_key(|(_, extra_width_count)| *extra_width_count)
}

fn synthesized_edge_error(
    profiles: &[ProfileData],
    total_bar_width: i32,
    observed_max_width: i32,
) -> i32 {
    let synthesized_max = profiles
        .iter()
        .flat_map(|profile| profile.pixel_map.keys())
        .filter_map(|width| width.parse::<i32>().ok())
        .filter(|width| *width >= MIN_DETECTABLE_WIDTH && *width < total_bar_width)
        .max()
        .unwrap_or(0);
    (synthesized_max - observed_max_width).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_versioned_format_loads() {
        let json = r#"{
            "format_version": 2,
            "detection_mode": "single",
            "timing_model": "open_interior_v1",
            "total_bar_width": 20,
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
        assert!(matches!(
            loaded.timing_model,
            CalibrationTimingModel::OpenInteriorV1 { .. }
        ));
    }

    #[test]
    fn old_flat_format_is_rejected() {
        // Pre-v2 single-profile flat format no longer loads.
        let json = r#"{
            "total_frames": 30,
            "pixel_map": {"0": 0, "5": 1, "10": 2}
        }"#;
        assert!(LoadedCalibration::from_json(json).is_err());
    }

    #[test]
    fn missing_format_version_is_rejected() {
        let json = r#"{
            "timing_model": "open_interior_v1",
            "total_bar_width": 180,
            "profiles": [{"total_frames": 30, "pixel_map": {"0": 0, "6": 1}}]
        }"#;
        assert!(LoadedCalibration::from_json(json).is_err());
    }

    #[test]
    fn missing_timing_model_is_rejected() {
        let json = r#"{
            "format_version": 2,
            "profiles": [{"total_frames": 30, "pixel_map": {"0": 0, "6": 1}}]
        }"#;
        assert!(LoadedCalibration::from_json(json).is_err());
    }

    #[test]
    fn test_open_interior_format() {
        let json = r#"{
            "format_version": 2,
            "timing_model": "open_interior_v1",
            "total_bar_width": 180,
            "boundary_switch_frame": 312,
            "profiles": [{
                "total_frames": 30,
                "pixel_map": {"0": 0, "6": 1, "12": 2}
            }]
        }"#;
        let loaded = LoadedCalibration::from_json(json).unwrap();
        assert_eq!(
            loaded.timing_model,
            CalibrationTimingModel::OpenInteriorV1 {
                total_bar_width: 180,
                boundary_switch_frame: 312
            }
        );
        assert_eq!(loaded.tables[0].total_frames, 30);
    }

    #[test]
    fn open_interior_requires_total_bar_width() {
        let json = r#"{
            "format_version": 2,
            "timing_model": "open_interior_v1",
            "profiles": [{
                "total_frames": 30,
                "pixel_map": {"0": 0}
            }]
        }"#;
        assert!(LoadedCalibration::from_json(json).is_err());
    }

    #[test]
    fn test_multi_profile() {
        let json = r#"{
            "format_version": 2,
            "detection_mode": "alternating",
            "timing_model": "open_interior_v1",
            "total_bar_width": 20,
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
        assert_eq!(
            data.timing_model,
            Some(TIMING_MODEL_OPEN_INTERIOR_V1.to_string())
        );
        assert_eq!(data.total_bar_width, Some(180));
        assert_eq!(
            data.boundary_switch_frame,
            Some(DEFAULT_BOUNDARY_SWITCH_FRAME)
        );
        assert_eq!(data.screen_width, Some(1920));
        assert_eq!(data.calibration_time, Some(123.0));
    }

    #[test]
    fn infer_calibration_uses_explicit_total_bar_width() {
        let samples = sample_cycles_for_n(157, 30.0);
        let data = infer_calibration_from_samples_with_ui_scaler_and_total_bar_width(
            &samples, 1920, 1080, 0.0, 157, None, 123.0,
        )
        .unwrap();

        assert_eq!(data.total_bar_width, Some(157));
        assert!(!data.profiles[0].pixel_map.contains_key("157"));
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

        assert_eq!(data.total_bar_width, Some(180));
        assert!(!data.profiles[0].pixel_map.contains_key("180"));
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
        let expected = synthesize_profiles(180, 180.0, 37.5);
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
        synthesize_profiles(total_bar_width, total_bar_width as f64, n_eff)
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
