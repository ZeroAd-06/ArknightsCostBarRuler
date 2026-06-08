/// Calibration data loading from JSON files.
/// Compatible with both old single-profile and new multi-profile formats.
use crate::analysis::mapping::CalibrationTable;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const SIMILARITY_THRESHOLD: f64 = 0.8;

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

pub fn build_calibration_from_samples(
    cycle_samples: &[Vec<i32>],
    screen_width: u32,
    screen_height: u32,
    calibration_time: f64,
) -> Result<CalibrationData, String> {
    if cycle_samples.is_empty() {
        return Err("未能收集到任何有效的费用条循环，请确保游戏处于慢速模式并重试。".to_string());
    }

    let mut clusters: Vec<Vec<Vec<i32>>> = Vec::new();
    for sample in cycle_samples {
        let sample_set: HashSet<i32> = sample.iter().copied().collect();
        if sample_set.is_empty() {
            continue;
        }

        let mut best_match_cluster_index = None;
        let mut max_similarity = -1.0;
        for (index, cluster) in clusters.iter().enumerate() {
            let representative_set: HashSet<i32> = cluster[0].iter().copied().collect();
            let similarity = jaccard_similarity(&sample_set, &representative_set);
            if similarity > max_similarity {
                max_similarity = similarity;
                best_match_cluster_index = Some(index);
            }
        }

        if max_similarity >= SIMILARITY_THRESHOLD {
            if let Some(index) = best_match_cluster_index {
                clusters[index].push(sample.clone());
            }
        } else {
            clusters.push(vec![sample.clone()]);
        }
    }

    let mut profiles = Vec::new();
    for cluster in clusters {
        let mut width_counts: HashMap<i32, usize> = HashMap::new();
        for width in cluster.into_iter().flatten() {
            *width_counts.entry(width).or_insert(0) += 1;
        }

        let count_zero = width_counts.get(&0).copied().unwrap_or(0) as f64;
        let non_zero_counts: Vec<f64> = width_counts
            .iter()
            .filter_map(|(width, count)| (*width > 0).then_some(*count as f64))
            .collect();
        let mut num_hidden_frames = 0;
        if !non_zero_counts.is_empty() {
            let median_count = median(non_zero_counts.clone());
            let outlier_threshold = median_count * 5.0;
            let filtered_counts: Vec<f64> = non_zero_counts
                .into_iter()
                .filter(|count| *count < outlier_threshold)
                .collect();
            if !filtered_counts.is_empty() {
                let baseline_frequency = median(filtered_counts);
                if baseline_frequency > 0.0 {
                    let num_frames_in_empty_state =
                        (count_zero / baseline_frequency).round() as i32;
                    num_hidden_frames = (num_frames_in_empty_state - 1).max(0);
                }
            }
        }

        let mut unique_pixel_widths: Vec<i32> = width_counts.keys().copied().collect();
        unique_pixel_widths.sort_unstable();
        let total_frames = unique_pixel_widths.len() as i32 + num_hidden_frames;
        let mut pixel_map = HashMap::new();
        if unique_pixel_widths.binary_search(&0).is_ok() {
            pixel_map.insert("0".to_string(), 0);
        }

        let frame_offset = 1 + num_hidden_frames;
        for (index, pixel_width) in unique_pixel_widths
            .into_iter()
            .filter(|width| *width > 0)
            .enumerate()
        {
            pixel_map.insert(pixel_width.to_string(), index as i32 + frame_offset);
        }

        profiles.push(ProfileData {
            total_frames,
            pixel_map,
        });
    }

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

fn jaccard_similarity(left: &HashSet<i32>, right: &HashSet<i32>) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection_size = left.intersection(right).count() as f64;
    let union_size = left.union(right).count() as f64;
    intersection_size / union_size
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
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

    #[test]
    fn build_calibration_detects_single_profile() {
        let samples = vec![vec![0, 5, 10, 15], vec![0, 5, 10, 15], vec![0, 5, 10, 15]];
        let data = build_calibration_from_samples(&samples, 1920, 1080, 123.0).unwrap();
        assert_eq!(data.detection_mode, Some("single".to_string()));
        assert_eq!(data.profiles.len(), 1);
        assert_eq!(data.profiles[0].total_frames, 4);
        assert_eq!(data.profiles[0].pixel_map.get("0"), Some(&0));
        assert_eq!(data.profiles[0].pixel_map.get("5"), Some(&1));
        assert_eq!(data.screen_width, Some(1920));
        assert_eq!(data.calibration_time, Some(123.0));
    }

    #[test]
    fn build_calibration_clusters_alternating_profiles() {
        let samples = vec![
            vec![0, 4, 8, 12],
            vec![0, 5, 10, 15, 20],
            vec![0, 4, 8, 12],
            vec![0, 5, 10, 15, 20],
        ];
        let data = build_calibration_from_samples(&samples, 1920, 1080, 123.0).unwrap();
        assert_eq!(data.detection_mode, Some("alternating".to_string()));
        assert_eq!(data.profiles.len(), 2);
    }
}
