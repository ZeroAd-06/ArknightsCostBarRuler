//! Pure overlay geometry & animation math: reset-cover animation phases,
//! calibration-progress easing, transparent-area hit testing, and initial
//! window placement. No window handles or Slint state here — just numbers,
//! which keeps the whole module unit-testable.

use ruler_core::analysis::roi::find_cost_bar_roi;
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

use super::{
    LOGICAL_H, LOGICAL_PANEL_H, LOGICAL_TOOLBAR_H, LOGICAL_TOOLBAR_RIGHT_PAD,
    LOGICAL_TOOLBAR_TOP_GAP, LOGICAL_TOOLBAR_W, LOGICAL_W,
};

// Reset cover animation timing (compact ~0.7s total). All three phases
// use a smoothstep curve so motion eases in/out non-linearly.
const RESET_EXPAND_MS: u128 = 180;
const RESET_HOLD_MS: u128 = 260;
const RESET_CONTRACT_MS: u128 = 240;

pub(super) struct ResetPhase {
    pub(super) cover: f32,
    pub(super) top: f32,
    pub(super) text_opacity: f32,
    pub(super) done: bool,
}

/// Cubic smoothstep: 0 at t=0, 1 at t=1, zero slope at both ends.
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Compute the cover/top/text-opacity for a reset animation at `elapsed`.
///
/// - Expand: bar grows top-down (top pinned at 0), text fades in with cover.
/// - Hold: full cover, full text.
/// - Contract: bar wipes downward off the bottom (bottom edge pinned, top
///   edge rises), text fades out as cover shrinks.
pub(super) fn compute_reset_phase(elapsed_ms: u128) -> ResetPhase {
    if elapsed_ms >= RESET_EXPAND_MS + RESET_HOLD_MS + RESET_CONTRACT_MS {
        return ResetPhase {
            cover: 0.0,
            top: 0.0,
            text_opacity: 0.0,
            done: true,
        };
    }
    if elapsed_ms < RESET_EXPAND_MS {
        let s = smoothstep(elapsed_ms as f32 / RESET_EXPAND_MS as f32);
        return ResetPhase {
            cover: s,
            top: 0.0,
            text_opacity: s,
            done: false,
        };
    }
    if elapsed_ms < RESET_EXPAND_MS + RESET_HOLD_MS {
        return ResetPhase {
            cover: 1.0,
            top: 0.0,
            text_opacity: 1.0,
            done: false,
        };
    }
    // Contract: cover and text recede as `smoothstep`, top rises so the
    // bottom edge (top + cover) stays pinned at 1.
    let s = smoothstep(
        (elapsed_ms - RESET_EXPAND_MS - RESET_HOLD_MS) as f32 / RESET_CONTRACT_MS as f32,
    );
    let cover = 1.0 - s;
    ResetPhase {
        cover,
        top: 1.0 - cover,
        text_opacity: cover,
        done: false,
    }
}

pub(super) fn advance_displayed_progress(displayed: f32, target: f32) -> f32 {
    let delta = target - displayed;
    if delta <= 0.0 {
        return displayed;
    }
    if delta < 0.35 {
        target
    } else {
        let step = (delta * 0.35).max(0.18).min(delta);
        displayed + step
    }
}

pub(super) fn outer_area_should_pass_through_at(x: i32, y: i32, scale: f32, running: bool) -> bool {
    if y < (LOGICAL_PANEL_H * scale).round() as i32 {
        return false;
    }
    if running && toolbar_hit_zone_contains(x as f32 / scale, y as f32 / scale) {
        return false;
    }
    true
}

fn toolbar_hit_zone_contains(logical_x: f32, logical_y: f32) -> bool {
    let left = LOGICAL_W - LOGICAL_TOOLBAR_W - LOGICAL_TOOLBAR_RIGHT_PAD;
    let right = LOGICAL_W - LOGICAL_TOOLBAR_RIGHT_PAD;
    let top = LOGICAL_PANEL_H;
    let bottom = LOGICAL_PANEL_H + LOGICAL_TOOLBAR_TOP_GAP + LOGICAL_TOOLBAR_H;

    logical_x >= left && logical_x < right && logical_y >= top && logical_y < bottom
}

/// Returns `(left, top, width, height, base_scale)` in physical pixels.
/// `base_scale` aligns the bar height to the legacy overlay footprint; the
/// effective scale is `base_scale * scale_mult`. A persisted `pos` is used
/// when present (clamped on-screen), otherwise it anchors bottom-right.
pub(super) fn initial_geometry(
    pos: Option<(i32, i32)>,
    scale_mult: f32,
) -> (i32, i32, i32, i32, f32) {
    unsafe {
        let screen_width = GetSystemMetrics(SM_CXSCREEN);
        let screen_height = GetSystemMetrics(SM_CYSCREEN);
        let (roi_x1, roi_x2, _) = find_cost_bar_roi(screen_width, screen_height);
        let cost_bar_pixel_length = (roi_x2 - roi_x1).abs().max(180);
        let legacy_height = (cost_bar_pixel_length * 5 / 6) * 27 / 50;
        let base_scale = (legacy_height as f32 / LOGICAL_PANEL_H).clamp(1.0, 4.0);
        let effective = base_scale * scale_mult;
        let width = (LOGICAL_W * effective).round() as i32;
        let height = (LOGICAL_H * effective).round() as i32;
        let (left, top) = match pos {
            Some((x, y)) => (
                x.clamp(0, (screen_width - width).max(0)),
                y.clamp(0, (screen_height - height).max(0)),
            ),
            None => (
                (screen_width - width - 50).max(0),
                (screen_height - height - 100).max(0),
            ),
        };
        (left, top, width, height, base_scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_toolbar_zone_stays_hit_testable_below_panel() {
        let scale = 2.5;
        let x = ((LOGICAL_W - LOGICAL_TOOLBAR_RIGHT_PAD - 10.0) * scale).round() as i32;
        let y = ((LOGICAL_PANEL_H + LOGICAL_TOOLBAR_TOP_GAP + 10.0) * scale).round() as i32;

        assert!(!outer_area_should_pass_through_at(x, y, scale, true));
    }

    #[test]
    fn leftmost_toolbar_button_zone_stays_hit_testable() {
        let scale = 2.5;
        let toolbar_left = LOGICAL_W - LOGICAL_TOOLBAR_W - LOGICAL_TOOLBAR_RIGHT_PAD;
        let x = ((toolbar_left + 12.0) * scale).round() as i32;
        let y = ((LOGICAL_PANEL_H + LOGICAL_TOOLBAR_TOP_GAP + 10.0) * scale).round() as i32;

        assert!(!outer_area_should_pass_through_at(x, y, scale, true));
    }

    #[test]
    fn running_toolbar_bridge_stays_hit_testable() {
        let scale = 2.5;
        let x = ((LOGICAL_W - LOGICAL_TOOLBAR_RIGHT_PAD - 10.0) * scale).round() as i32;
        let y = ((LOGICAL_PANEL_H + 1.0) * scale).round() as i32;

        assert!(!outer_area_should_pass_through_at(x, y, scale, true));
    }

    #[test]
    fn lower_transparent_area_outside_toolbar_passes_through() {
        let scale = 2.5;
        let x = (20.0_f32 * scale).round() as i32;
        let y = ((LOGICAL_PANEL_H + LOGICAL_TOOLBAR_TOP_GAP + 10.0) * scale).round() as i32;

        assert!(outer_area_should_pass_through_at(x, y, scale, true));
    }

    #[test]
    fn toolbar_zone_passes_through_when_not_running() {
        let scale = 2.5;
        let x = ((LOGICAL_W - LOGICAL_TOOLBAR_RIGHT_PAD - 10.0) * scale).round() as i32;
        let y = ((LOGICAL_PANEL_H + LOGICAL_TOOLBAR_TOP_GAP + 10.0) * scale).round() as i32;

        assert!(outer_area_should_pass_through_at(x, y, scale, false));
    }

    #[test]
    fn displayed_progress_advances_toward_target_without_overshoot() {
        let next = advance_displayed_progress(10.0, 20.0);

        assert!(next > 10.0);
        assert!(next < 20.0);
    }

    #[test]
    fn displayed_progress_does_not_rewind_when_target_jitters_down() {
        assert_eq!(advance_displayed_progress(20.0, 18.0), 20.0);
    }

    #[test]
    fn smoothstep_hits_endpoints_and_midpoint() {
        assert_eq!(smoothstep(0.0), 0.0);
        assert_eq!(smoothstep(1.0), 1.0);
        assert!((smoothstep(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn smoothstep_clamps_outside_unit_interval() {
        assert_eq!(smoothstep(-0.5), 0.0);
        assert_eq!(smoothstep(1.5), 1.0);
    }

    #[test]
    fn reset_phase_expand_starts_empty() {
        let p = compute_reset_phase(0);
        assert_eq!(p.cover, 0.0);
        assert_eq!(p.top, 0.0);
        assert_eq!(p.text_opacity, 0.0);
        assert!(!p.done);
    }

    #[test]
    fn reset_phase_expand_end_is_full() {
        // At the boundary the expand phase should report full cover.
        let p = compute_reset_phase(RESET_EXPAND_MS);
        assert!((p.cover - 1.0).abs() < 1e-6);
        assert_eq!(p.top, 0.0);
        assert!(!p.done);
    }

    #[test]
    fn reset_phase_hold_is_full_and_centered() {
        let mid_hold = RESET_EXPAND_MS + RESET_HOLD_MS / 2;
        let p = compute_reset_phase(mid_hold);
        assert_eq!(p.cover, 1.0);
        assert_eq!(p.top, 0.0);
        assert_eq!(p.text_opacity, 1.0);
        assert!(!p.done);
    }

    #[test]
    fn reset_phase_contract_keeps_bottom_edge_pinned() {
        // During the contract phase the bottom edge (top + cover) must
        // stay at 1 so the bar wipes downward without sliding.
        for ms in 0..RESET_CONTRACT_MS {
            let elapsed = RESET_EXPAND_MS + RESET_HOLD_MS + ms;
            let p = compute_reset_phase(elapsed);
            assert!(!p.done, "phase should not be done at {elapsed}ms");
            assert!(
                (p.top + p.cover - 1.0).abs() < 1e-5,
                "bottom edge drifted at {elapsed}ms: top={} cover={}",
                p.top,
                p.cover
            );
            assert!(p.cover >= 0.0 && p.cover <= 1.0);
        }
    }

    #[test]
    fn reset_phase_done_after_total_duration() {
        let total = RESET_EXPAND_MS + RESET_HOLD_MS + RESET_CONTRACT_MS;
        let p = compute_reset_phase(total);
        assert!(p.done);
    }
}
