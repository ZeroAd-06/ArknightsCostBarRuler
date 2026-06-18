/// Closed-form synthesis for Arknights cost-bar calibration profiles.
use super::calibration::ProfileData;
use std::collections::{BTreeSet, HashMap};

pub const BAR_LENGTH_RATIO: f64 = 1.03892;
pub const BASE_FRAMES_PER_COST: f64 = 30.0;
pub const MIN_DETECTABLE_WIDTH: i32 = 2;

const EPSILON: f64 = 1e-6;
const MAX_PROFILE_PERIOD: i32 = 60;

pub fn synthesized_width(total_bar_width: i32, n_eff: f64, global_frame: i32) -> i32 {
    if total_bar_width <= 0 || n_eff <= 0.0 || !n_eff.is_finite() {
        return 0;
    }

    let visible_width = total_bar_width as f64;
    let bar_length = BAR_LENGTH_RATIO * visible_width;
    let hidden_width = bar_length - visible_width;
    let phase = normalized_phase(global_frame as f64 / n_eff);
    let raw_width = (bar_length * phase - hidden_width).round() as i32 + 1;

    if raw_width < MIN_DETECTABLE_WIDTH {
        0
    } else {
        raw_width.clamp(0, total_bar_width)
    }
}

pub fn synthesized_widths(total_bar_width: i32, n_eff: f64) -> BTreeSet<i32> {
    synthesize_profiles(total_bar_width, n_eff)
        .into_iter()
        .flat_map(|profile| {
            profile
                .pixel_map
                .into_keys()
                .filter_map(|width| width.parse::<i32>().ok())
        })
        .collect()
}

pub fn synthesize_profiles(total_bar_width: i32, n_eff: f64) -> Vec<ProfileData> {
    if total_bar_width <= 0 || n_eff <= 0.0 || !n_eff.is_finite() {
        return Vec::new();
    }

    let profile_count = profile_period_for_n_eff(n_eff);
    (0..profile_count)
        .map(|cycle_index| synthesize_cycle_profile(total_bar_width, n_eff, cycle_index))
        .collect()
}

pub fn profile_period_for_n_eff(n_eff: f64) -> i32 {
    if n_eff <= 0.0 || !n_eff.is_finite() {
        return 0;
    }

    for denominator in 1..=MAX_PROFILE_PERIOD {
        let numerator = (n_eff * denominator as f64).round() as i64;
        if numerator <= 0 {
            continue;
        }
        let approximated = numerator as f64 / denominator as f64;
        if (approximated - n_eff).abs() < EPSILON {
            return denominator / gcd_i32(numerator.unsigned_abs() as i32, denominator);
        }
    }

    1
}

pub fn is_half_frame_n(n_eff: f64) -> bool {
    let doubled = n_eff * 2.0;
    let rounded = doubled.round();
    (rounded - doubled).abs() < EPSILON && rounded as i64 % 2 != 0
}

fn synthesize_cycle_profile(total_bar_width: i32, n_eff: f64, cycle_index: i32) -> ProfileData {
    let start_frame = ceil_frame(cycle_index as f64 * n_eff);
    let end_frame = ceil_frame((cycle_index + 1) as f64 * n_eff);
    let total_frames = (end_frame - start_frame).max(1);
    let mut pixel_map = HashMap::new();

    for global_frame in start_frame..end_frame {
        let local_frame = global_frame - start_frame;
        let width = synthesized_width(total_bar_width, n_eff, global_frame);
        pixel_map.entry(width.to_string()).or_insert(local_frame);
    }
    pixel_map
        .entry(total_bar_width.to_string())
        .or_insert(total_frames - 1);

    ProfileData {
        total_frames,
        pixel_map,
    }
}

fn normalized_phase(cycles: f64) -> f64 {
    let phase = cycles - cycles.floor();
    if phase.abs() < EPSILON || (1.0 - phase).abs() < EPSILON {
        0.0
    } else {
        phase
    }
}

fn ceil_frame(value: f64) -> i32 {
    let rounded = value.round();
    if (rounded - value).abs() < EPSILON {
        rounded as i32
    } else {
        value.ceil() as i32
    }
}

fn gcd_i32(mut a: i32, mut b: i32) -> i32 {
    while b != 0 {
        let rem = a % b;
        a = b;
        b = rem;
    }
    a.abs().max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesizes_single_integer_profile() {
        let profiles = synthesize_profiles(180, 30.0);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].total_frames, 30);
        assert_eq!(profiles[0].pixel_map.get("0"), Some(&0));
        assert!(profiles[0].pixel_map.contains_key("180"));
    }

    #[test]
    fn synthesizes_expected_known_widths() {
        assert_eq!(synthesized_width(180, 30.0, 0), 0);
        assert_eq!(synthesized_width(180, 30.0, 1), 0);
        assert_eq!(synthesized_width(180, 30.0, 2), 6);
        assert_eq!(synthesized_width(180, 30.0, 29), 175);
    }

    #[test]
    fn synthesizes_half_frame_alternating_profiles() {
        let profiles = synthesize_profiles(180, 37.5);
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].total_frames, 38);
        assert_eq!(profiles[1].total_frames, 37);
        assert_ne!(profiles[0].pixel_map, profiles[1].pixel_map);
    }

    #[test]
    fn synthesizes_repeating_profiles_for_tenth_speed_bonus() {
        let profiles = synthesize_profiles(120, 30.0 / 1.1);
        assert_eq!(profiles.len(), 11);
        assert_eq!(
            profiles
                .iter()
                .map(|profile| profile.total_frames)
                .sum::<i32>(),
            300
        );
        assert!(profiles.iter().any(|profile| profile.total_frames == 27));
        assert!(profiles.iter().any(|profile| profile.total_frames == 28));
    }

    #[test]
    fn width_union_contains_both_half_frame_profiles() {
        let profiles = synthesize_profiles(180, 37.5);
        let widths = synthesized_widths(180, 37.5);
        for profile in profiles {
            for width in profile.pixel_map.keys() {
                let width = width.parse::<i32>().unwrap();
                assert!(widths.contains(&width));
            }
        }
    }

    #[test]
    fn detects_only_x_point_five_as_half_frame() {
        assert!(is_half_frame_n(37.5));
        assert!(is_half_frame_n(15.5));
        assert!(!is_half_frame_n(30.0));
        assert!(!is_half_frame_n(37.0));
    }

    #[test]
    fn detects_profile_period_from_small_rational_denominators() {
        assert_eq!(profile_period_for_n_eff(30.0), 1);
        assert_eq!(profile_period_for_n_eff(37.5), 2);
        assert_eq!(profile_period_for_n_eff(30.0 / 1.1), 11);
    }
}
