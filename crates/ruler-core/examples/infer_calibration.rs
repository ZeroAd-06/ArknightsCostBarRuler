//! 离线校验「正常速度样本推断 N → 公式合成 pixel_map」的新校准逻辑。
//!
//! 用法:
//!   cargo run --release -p ruler-core --example infer_calibration
//!
//! 默认读取工作区根目录下的 cost_data/*_raw.csv 及对应 *_meta.json,并只使用前 2 个完整循环验证。

use ruler_core::analysis::calibration::{infer_calibration_from_samples, CalibrationData};
use ruler_core::analysis::roi::{
    cost_bar_width_frac_with_ui_scaler, find_cost_bar_roi, DEFAULT_UI_SCALER,
};
use ruler_core::analysis::synthesis::{
    profile_period_for_n_eff, synthesize_profiles, BASE_FRAMES_PER_COST, MIN_DETECTABLE_WIDTH,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const VALIDATION_CYCLES: usize = 2;

#[derive(Debug, Deserialize)]
struct MetaData {
    label: String,
    screen_width: u32,
    screen_height: u32,
    total_bar_width: i32,
}

struct ValidationSummary {
    label: String,
    screen_width: u32,
    screen_height: u32,
    expected_n: f64,
    inferred_n: f64,
    detection_mode: String,
    cycle_count: usize,
    reliable_width_count: usize,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("错误: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let data_dir = locate_cost_data_dir()?;
    let mut raw_files = fs::read_dir(&data_dir)
        .map_err(|e| format!("无法读取 {}: {e}", data_dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("_raw.csv"))
        })
        .collect::<Vec<_>>();
    raw_files.sort();

    if raw_files.is_empty() {
        return Err(format!("{} 中没有 *_raw.csv", data_dir.display()));
    }

    println!(
        "{:<16} {:>11} {:>8} {:>8} {:<12} {:>6} {:>8}",
        "label", "screen", "expect", "infer", "mode", "cycles", "widths"
    );
    println!("{}", "-".repeat(78));

    let mut passed = 0usize;
    let mut failed = 0usize;
    for raw_path in raw_files {
        match validate_dataset(&raw_path) {
            Ok(summary) => {
                passed += 1;
                println!(
                    "{:<16} {:>5}x{:<5} {:>8.1} {:>8.1} {:<12} {:>6} {:>8}  PASS",
                    summary.label,
                    summary.screen_width,
                    summary.screen_height,
                    summary.expected_n,
                    summary.inferred_n,
                    summary.detection_mode,
                    summary.cycle_count,
                    summary.reliable_width_count
                );
            }
            Err(error) => {
                failed += 1;
                let label = raw_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("<unknown>");
                println!("{label:<16} FAIL: {error}");
            }
        }
    }

    if failed > 0 {
        Err(format!("{passed} 组通过，{failed} 组失败"))
    } else {
        println!("{}", "-".repeat(78));
        println!("全部 {passed} 组数据通过：N 正确、模式正确、可靠宽度 0px 复现。");
        Ok(())
    }
}

fn validate_dataset(raw_path: &Path) -> Result<ValidationSummary, String> {
    let meta = read_meta(raw_path)?;
    let (x1, x2, _) = find_cost_bar_roi(meta.screen_width as i32, meta.screen_height as i32);
    let roi_width = x2 - x1;
    if roi_width != meta.total_bar_width {
        return Err(format!(
            "meta 条宽 {} 与 ROI 条宽 {} 不一致",
            meta.total_bar_width, roi_width
        ));
    }

    let cycles = read_raw_cycles(raw_path)?
        .into_iter()
        .take(VALIDATION_CYCLES)
        .collect::<Vec<_>>();
    let calibration =
        infer_calibration_from_samples(&cycles, meta.screen_width, meta.screen_height, 0.0)?;

    let expected_n = expected_n_from_label(&meta.label)?;
    let inferred_n = calibration.n_eff();
    if (expected_n - inferred_n).abs() > 0.001 {
        return Err(format!(
            "N 不匹配：期望 {expected_n:.3}，推断 {inferred_n:.3}"
        ));
    }

    let detection_mode = if profile_period_for_n_eff(inferred_n) > 1 {
        "alternating".to_string()
    } else {
        "single".to_string()
    };
    let should_alternate = meta.label.contains("80%");
    if should_alternate && detection_mode != "alternating" {
        return Err(format!("期望 alternating，但得到 {detection_mode}"));
    }
    if !should_alternate && detection_mode != "single" {
        return Err(format!("期望 single，但得到 {detection_mode}"));
    }

    let reliable_cycles = collect_reliable_cycles(&cycles, meta.total_bar_width);
    if reliable_cycles.is_empty() {
        return Err("没有可靠宽度可验证".to_string());
    }
    let reliable_width_count = reliable_cycles.iter().map(BTreeSet::len).sum();
    assert_profile_reproduces_cycles(
        &calibration,
        meta.total_bar_width,
        cost_bar_width_frac_with_ui_scaler(
            meta.screen_width as i32,
            meta.screen_height as i32,
            DEFAULT_UI_SCALER,
        ),
        &reliable_cycles,
    )?;

    Ok(ValidationSummary {
        label: meta.label,
        screen_width: meta.screen_width,
        screen_height: meta.screen_height,
        expected_n,
        inferred_n,
        detection_mode,
        cycle_count: reliable_cycles.len(),
        reliable_width_count,
    })
}

fn locate_cost_data_dir() -> Result<PathBuf, String> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        PathBuf::from("cost_data"),
        manifest_dir.join("..").join("..").join("cost_data"),
    ];

    candidates
        .into_iter()
        .find(|path| path.is_dir())
        .ok_or_else(|| "找不到 cost_data 目录，请在工作区根目录运行该示例".to_string())
}

fn read_meta(raw_path: &Path) -> Result<MetaData, String> {
    let raw_name = raw_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("非法文件名：{}", raw_path.display()))?;
    let prefix = raw_name
        .strip_suffix("_raw.csv")
        .ok_or_else(|| format!("不是 raw CSV：{}", raw_path.display()))?;
    let meta_path = raw_path.with_file_name(format!("{prefix}_meta.json"));
    let content = fs::read_to_string(&meta_path)
        .map_err(|e| format!("无法读取 {}: {e}", meta_path.display()))?;
    serde_json::from_str(&content).map_err(|e| format!("无法解析 {}: {e}", meta_path.display()))
}

fn read_raw_cycles(raw_path: &Path) -> Result<Vec<Vec<i32>>, String> {
    let content = fs::read_to_string(raw_path)
        .map_err(|e| format!("无法读取 {}: {e}", raw_path.display()))?;
    let mut cycles: BTreeMap<i32, Vec<i32>> = BTreeMap::new();

    for (line_index, line) in content.lines().enumerate().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts = line.split(',').collect::<Vec<_>>();
        if parts.len() != 3 {
            return Err(format!(
                "{}:{} 列数错误：{}",
                raw_path.display(),
                line_index + 1,
                line
            ));
        }
        let cycle_index = parts[0].parse::<i32>().map_err(|e| {
            format!(
                "{}:{} cycle_index 解析失败：{e}",
                raw_path.display(),
                line_index + 1
            )
        })?;
        if cycle_index < 0 {
            continue;
        }
        let pixel_width = parts[2].parse::<i32>().map_err(|e| {
            format!(
                "{}:{} pixel_width 解析失败：{e}",
                raw_path.display(),
                line_index + 1
            )
        })?;
        cycles.entry(cycle_index).or_default().push(pixel_width);
    }

    let cycles = cycles.into_values().collect::<Vec<_>>();
    if cycles.is_empty() {
        Err(format!("{} 中没有完整 cycle", raw_path.display()))
    } else {
        Ok(cycles)
    }
}

fn expected_n_from_label(label: &str) -> Result<f64, String> {
    let percent = percent_from_label(label)?;
    if (percent - 33.0).abs() < 0.01 {
        Ok(90.0)
    } else {
        Ok(BASE_FRAMES_PER_COST * 100.0 / percent)
    }
}

fn percent_from_label(label: &str) -> Result<f64, String> {
    let percent_pos = label
        .rfind('%')
        .ok_or_else(|| format!("label 中没有 %：{label}"))?;
    let prefix = &label[..percent_pos];
    let mut start = percent_pos;
    for (index, ch) in prefix.char_indices().rev() {
        if ch.is_ascii_digit() || ch == '.' {
            start = index;
        } else {
            break;
        }
    }
    if start == percent_pos {
        return Err(format!("label 中没有百分比数字：{label}"));
    }
    label[start..percent_pos]
        .parse::<f64>()
        .map_err(|e| format!("label 百分比解析失败 {label}: {e}"))
}

fn collect_reliable_cycles(cycles: &[Vec<i32>], total_bar_width: i32) -> Vec<BTreeSet<i32>> {
    cycles
        .iter()
        .filter_map(|cycle| {
            let widths = cycle
                .iter()
                .copied()
                .filter(|width| *width >= MIN_DETECTABLE_WIDTH && *width < total_bar_width)
                .collect::<BTreeSet<_>>();
            (!widths.is_empty()).then_some(widths)
        })
        .collect()
}

fn assert_profile_reproduces_cycles(
    calibration: &CalibrationData,
    total_bar_width: i32,
    bar_width_frac: f64,
    reliable_cycles: &[BTreeSet<i32>],
) -> Result<(), String> {
    let generated = synthesize_profiles(total_bar_width, bar_width_frac, calibration.n_eff());
    let profile_sets = generated
        .iter()
        .map(|profile| {
            profile
                .pixel_map
                .keys()
                .filter_map(|width| width.parse::<i32>().ok())
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();

    if profile_sets.is_empty() {
        return Err("校准结果没有 profile".to_string());
    }

    let matching_offset = (0..profile_sets.len()).find(|offset| {
        reliable_cycles
            .iter()
            .enumerate()
            .all(|(cycle_index, cycle)| {
                cycle.is_subset(&profile_sets[(cycle_index + offset) % profile_sets.len()])
            })
    });

    if matching_offset.is_some() {
        return Ok(());
    }

    for (cycle_index, cycle) in reliable_cycles.iter().enumerate() {
        let profile = &profile_sets[cycle_index % profile_sets.len()];
        if let Some(missing) = cycle.iter().find(|width| !profile.contains(width)) {
            return Err(format!(
                "cycle {cycle_index} 的宽度 {missing} 不在合成 profile 中"
            ));
        }
    }

    Err("可靠宽度无法被合成 profile 0px 复现".to_string())
}
