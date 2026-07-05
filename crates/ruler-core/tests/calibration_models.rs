use ruler_core::analysis::calibration::{infer_calibration_from_samples, CalibrationData};
use ruler_core::analysis::roi::{
    cost_bar_width_frac_with_ui_scaler, find_cost_bar_roi, DEFAULT_UI_SCALER,
};
use ruler_core::analysis::synthesis::{
    synthesize_profiles, SynthesizedProfile, BASE_FRAMES_PER_COST, MIN_DETECTABLE_WIDTH,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct CostDataMeta {
    label: String,
    screen_width: u32,
    screen_height: u32,
    total_bar_width: i32,
}

#[derive(Debug, Deserialize)]
struct RecordedModel {
    label: String,
    n_eff: [i32; 2],
    sequences: Vec<Vec<String>>,
}

#[test]
fn tracked_cost_data_infers_expected_models_without_missing_widths() {
    let data_dir = repo_root().join("cost_data");
    let mut raw_files = fs::read_dir(&data_dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", data_dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("_raw.csv"))
        })
        .collect::<Vec<_>>();
    raw_files.sort();
    assert!(
        !raw_files.is_empty(),
        "no tracked cost_data raw CSV files found"
    );

    for raw_path in raw_files {
        let meta = read_meta(&raw_path);
        let (_, x2, _) = find_cost_bar_roi(meta.screen_width as i32, meta.screen_height as i32);
        let (x1, _, _) = find_cost_bar_roi(meta.screen_width as i32, meta.screen_height as i32);
        assert_eq!(
            meta.total_bar_width,
            x2 - x1,
            "{} ROI width drifted",
            raw_path.display()
        );

        let cycles = read_raw_cycles(&raw_path)
            .into_iter()
            .take(2)
            .collect::<Vec<_>>();
        let calibration =
            infer_calibration_from_samples(&cycles, meta.screen_width, meta.screen_height, 0.0)
                .unwrap_or_else(|error| panic!("{} failed inference: {error}", raw_path.display()));
        let expected_n = expected_n_from_label(&meta.label);
        let inferred_n = inferred_n(&calibration);
        assert!(
            (expected_n - inferred_n).abs() < 0.001,
            "{} expected N={expected_n}, inferred N={inferred_n}",
            raw_path.display()
        );

        let reliable_cycles = reliable_cycles(&cycles, meta.total_bar_width);
        let generated = synthesize_profiles(
            meta.total_bar_width,
            cost_bar_width_frac_with_ui_scaler(
                meta.screen_width as i32,
                meta.screen_height as i32,
                DEFAULT_UI_SCALER,
            ),
            calibration.n_eff(),
        );
        assert_sequences_fit_profiles(&meta.label, &[reliable_cycles], &generated);
    }
}

#[test]
fn recorded_debug_models_match_synthesis_without_missing_widths() {
    let models: Vec<RecordedModel> =
        serde_json::from_str(include_str!("fixtures/recorded_cost_models.json")).unwrap();
    assert!(!models.is_empty());

    for model in models {
        let n_eff = model.n_eff[0] as f64 / model.n_eff[1] as f64;
        let profiles = synthesize_profiles(120, 120.0, n_eff);
        assert!(!profiles.is_empty(), "{} produced no profiles", model.label);

        for (sequence_index, sequence) in model.sequences.iter().enumerate() {
            let cycles = sequence
                .iter()
                .map(|widths| parse_width_set(widths))
                .collect::<Vec<_>>();
            assert_sequences_fit_profiles(
                &format!("{} sequence {sequence_index}", model.label),
                &[cycles],
                &profiles,
            );
        }
    }
}

#[test]
fn recorded_debug_models_infer_expected_n() {
    let models: Vec<RecordedModel> =
        serde_json::from_str(include_str!("fixtures/recorded_cost_models.json")).unwrap();

    for model in models {
        let expected_n = model.n_eff[0] as f64 / model.n_eff[1] as f64;
        for (sequence_index, sequence) in model.sequences.iter().enumerate() {
            let cycles = sequence
                .iter()
                .map(|widths| {
                    let mut cycle = parse_width_set(widths).into_iter().collect::<Vec<_>>();
                    cycle.insert(0, 0);
                    cycle.push(120);
                    cycle
                })
                .collect::<Vec<_>>();
            let calibration = infer_calibration_from_samples(&cycles, 1280, 720, 0.0)
                .unwrap_or_else(|error| {
                    panic!(
                        "{} sequence {sequence_index} failed inference: {error}",
                        model.label
                    )
                });
            let inferred_n = inferred_n(&calibration);
            assert!(
                (expected_n - inferred_n).abs() < 0.001,
                "{} sequence {sequence_index} expected N={expected_n}, inferred N={inferred_n}",
                model.label
            );
        }
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn read_meta(raw_path: &Path) -> CostDataMeta {
    let raw_name = raw_path.file_name().and_then(|name| name.to_str()).unwrap();
    let prefix = raw_name.strip_suffix("_raw.csv").unwrap();
    let meta_path = raw_path.with_file_name(format!("{prefix}_meta.json"));
    let content = fs::read_to_string(&meta_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", meta_path.display()));
    serde_json::from_str(&content)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", meta_path.display()))
}

fn read_raw_cycles(raw_path: &Path) -> Vec<Vec<i32>> {
    let content = fs::read_to_string(raw_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", raw_path.display()));
    let mut cycles: BTreeMap<i32, Vec<i32>> = BTreeMap::new();

    for (line_index, line) in content.lines().enumerate().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts = line.split(',').collect::<Vec<_>>();
        assert_eq!(
            parts.len(),
            3,
            "{}:{} bad row",
            raw_path.display(),
            line_index + 1
        );
        let cycle_index = parts[0].parse::<i32>().unwrap();
        if cycle_index < 0 || parts[2].is_empty() {
            continue;
        }
        let pixel_width = parts[2].parse::<i32>().unwrap();
        cycles.entry(cycle_index).or_default().push(pixel_width);
    }

    cycles.into_values().collect()
}

fn expected_n_from_label(label: &str) -> f64 {
    let percent = percent_from_label(label);
    if (percent - 33.0).abs() < 0.01 {
        90.0
    } else {
        BASE_FRAMES_PER_COST * 100.0 / percent
    }
}

fn percent_from_label(label: &str) -> f64 {
    let percent_pos = label
        .rfind('%')
        .unwrap_or_else(|| panic!("label has no %: {label}"));
    let prefix = &label[..percent_pos];
    let mut start = percent_pos;
    for (index, ch) in prefix.char_indices().rev() {
        if ch.is_ascii_digit() || ch == '.' {
            start = index;
        } else {
            break;
        }
    }
    label[start..percent_pos].parse::<f64>().unwrap()
}

fn inferred_n(calibration: &CalibrationData) -> f64 {
    calibration.n_eff()
}

fn reliable_cycles(cycles: &[Vec<i32>], total_bar_width: i32) -> Vec<BTreeSet<i32>> {
    cycles
        .iter()
        .map(|cycle| {
            cycle
                .iter()
                .copied()
                .filter(|width| *width >= MIN_DETECTABLE_WIDTH && *width < total_bar_width)
                .collect::<BTreeSet<_>>()
        })
        .filter(|cycle| !cycle.is_empty())
        .collect()
}

fn parse_width_set(widths: &str) -> BTreeSet<i32> {
    widths
        .split_whitespace()
        .map(|width| width.parse::<i32>().unwrap())
        .collect()
}

fn assert_sequences_fit_profiles(
    label: &str,
    sequences: &[Vec<BTreeSet<i32>>],
    profiles: &[SynthesizedProfile],
) {
    let profile_sets = profiles
        .iter()
        .map(|profile| {
            profile
                .pixel_map
                .keys()
                .map(|width| width.parse::<i32>().unwrap())
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();
    assert!(!profile_sets.is_empty(), "{label} has no profiles");

    for sequence in sequences {
        let matching_offset = (0..profile_sets.len()).find(|offset| {
            sequence.iter().enumerate().all(|(cycle_index, cycle)| {
                cycle.is_subset(&profile_sets[(cycle_index + *offset) % profile_sets.len()])
            })
        });
        if matching_offset.is_none() {
            let mut best_missing = Vec::new();
            for offset in 0..profile_sets.len() {
                let missing = sequence
                    .iter()
                    .enumerate()
                    .flat_map(|(cycle_index, cycle)| {
                        cycle
                            .difference(&profile_sets[(cycle_index + offset) % profile_sets.len()])
                            .map(move |width| (cycle_index, *width))
                    })
                    .collect::<Vec<_>>();
                if best_missing.is_empty() || missing.len() < best_missing.len() {
                    best_missing = missing;
                }
            }
            panic!("{label} missing widths: {best_missing:?}");
        }
    }
}
