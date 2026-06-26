//! Push the [`WizardCore`] state into the Slint `Wizard` component each render
//! tick. Every section is change-gated (signatures / tokens) so a 60 Hz repaint
//! stays cheap: rows update in place, the preview is rebuilt only when a new
//! probe frame arrives, and the live preview is box-downscaled here so the
//! software renderer never rescales a full game frame per repaint.

use ruler_core::PixelFormat;
use slint::{Image, Model, Rgba8Pixel, SharedPixelBuffer};

use super::WizardCore;
use crate::target_discovery::{LatencyClass, PreviewFrame, TargetCandidate};
use crate::ui::{TargetRow, Wizard};

// Fixed logical design height of `wizard.slint` (physical = logical * scale).
pub(super) const WIZARD_LOGICAL_H: f32 = 443.0;
// Extra logical height for the debug panel. Two budgets: the recording-only
// section (manual target off) and the taller recording + manual-target panel
// (sized for the dropdown-open type list / the 3-field MuMu-LDPlayer case).
const WIZARD_DEBUG_RECORDING_H: f32 = 150.0;
const WIZARD_DEBUG_MANUAL_H: f32 = 300.0;

pub(super) fn wizard_logical_height(debug_expanded: bool, manual_mode: bool) -> f32 {
    if !debug_expanded {
        WIZARD_LOGICAL_H
    } else if manual_mode {
        WIZARD_LOGICAL_H + WIZARD_DEBUG_MANUAL_H
    } else {
        WIZARD_LOGICAL_H + WIZARD_DEBUG_RECORDING_H
    }
}

/// Push the current candidate state into the Slint component. Each section
/// is change-gated so the render tick stays cheap: rows update in place, the
/// preview is rebuilt only when a new frame arrives for the selection, and
/// the header/status assignments rely on Slint's own equality check.
pub(super) fn sync_to_slint(wizard: &Wizard, core: &mut WizardCore) {
    // The manual row only exists while the debug panel is expanded. Collapsing
    // the panel therefore also exits manual mode (its UI is gone).
    let debug_expanded = wizard.get_debug_expanded();
    if wizard.get_show_manual_row() != debug_expanded {
        wizard.set_show_manual_row(debug_expanded);
    }
    if !debug_expanded && core.manual_mode {
        core.manual_mode = false;
        core.manual_error = None;
        core.preview_token = None;
    }

    sync_rows(wizard, core);
    if core.manual_mode {
        sync_manual(wizard, core);
    } else {
        if wizard.get_manual_selected() {
            wizard.set_manual_selected(false);
        }
        if wizard.get_manual_kind_open() {
            wizard.set_manual_kind_open(false);
        }
        sync_preview(wizard, core);
        sync_header_status(wizard, core);
    }
    sync_adb_availability(wizard);
    sync_update_notice(wizard, core);
}

fn sync_update_notice(wizard: &Wizard, core: &WizardCore) {
    let notice = core.app_state.snapshot().ui.update_notice;
    let available = notice.is_some();
    if wizard.get_update_available() != available {
        wizard.set_update_available(available);
    }
    let Some(notice) = notice else {
        return;
    };
    let version = format!("v{}", notice.version.trim_start_matches('v'));
    let text = core
        .i18n
        .tr_with("update.badge.wizard", &[("version", version)]);
    if wizard.get_update_text() != text {
        wizard.set_update_text(text.into());
    }
}

/// Render the manual-target state: the manual row is highlighted, there is no
/// live preview, and the status line shows a hint (or the last validation
/// error). The typed parameter values live in the Slint properties.
fn sync_manual(wizard: &Wizard, core: &WizardCore) {
    if !wizard.get_manual_selected() {
        wizard.set_manual_selected(true);
    }
    wizard.set_has_preview(false);
    wizard.set_header_text(core.i18n.tr("config.selector.manual_header").into());
    match &core.manual_error {
        Some(error) => {
            wizard.set_status_text(error.clone().into());
            wizard.set_status_error(true);
        }
        None => {
            wizard.set_status_text(core.i18n.tr("config.selector.manual_hint").into());
            wizard.set_status_error(false);
        }
    }
}

/// Reflect the cached adb resolution onto the wizard's banner. The
/// resolver is updated by `run_config_wizard` at startup and by the
/// refresh callback (which re-runs `resolve_adb_with` in case the user
/// started an emulator after the wizard opened).
fn sync_adb_availability(wizard: &Wizard) {
    let available = ruler_core::capture::adb_resolver::adb_available();
    let currently_shown = wizard.get_adb_unavailable();
    let should_show = !available;
    if currently_shown != should_show {
        wizard.set_adb_unavailable(should_show);
    }
}

/// Reconcile the target list. On a structural change (a different candidate
/// set or order) the model is reset; otherwise only the cells that actually
/// changed are written, leaving the repeater — and its row hover animations —
/// intact.
fn sync_rows(wizard: &Wizard, core: &mut WizardCore) {
    let content_sig = rows_signature(&core.candidates, core.selected_index);
    if core.rows_content_sig.as_deref() == Some(content_sig.as_str()) {
        return;
    }

    let rows: Vec<TargetRow> = core
        .candidates
        .iter()
        .enumerate()
        .map(|(idx, candidate)| target_row(candidate, core.selected_index == Some(idx)))
        .collect();

    let struct_sig = rows_struct_signature(&core.candidates);
    if core.rows_struct_sig != struct_sig {
        core.rows_model.set_vec(rows);
        core.rows_struct_sig = struct_sig;
    } else {
        for (idx, row) in rows.into_iter().enumerate() {
            if core.rows_model.row_data(idx).as_ref() != Some(&row) {
                core.rows_model.set_row_data(idx, row);
            }
        }
    }

    wizard.set_scanning(core.candidates.is_empty());
    wizard.set_cap_empty(core.i18n.tr("config.selector.no_targets").into());
    core.rows_content_sig = Some(content_sig);
}

fn target_row(candidate: &TargetCandidate, selected: bool) -> TargetRow {
    let error = candidate.error.is_some();
    let latency = candidate
        .error
        .clone()
        .unwrap_or_else(|| candidate.latency_text());
    TargetRow {
        name: candidate.name.as_str().into(),
        detail: candidate.detail.as_str().into(),
        latency: latency.into(),
        latency_class: latency_class_index(candidate.latency_class),
        error,
        selected,
    }
}

/// Rebuild the preview only when the selected target's frame pointer changes
/// (a new probe frame arrived) or the selection moves.
fn sync_preview(wizard: &Wizard, core: &mut WizardCore) {
    let token = core
        .selected_candidate()
        .and_then(|candidate| candidate.preview.as_ref())
        .map(|preview| {
            (
                core.selected_index.unwrap_or(usize::MAX),
                preview.data.as_ptr() as usize,
                preview.data.len(),
            )
        });
    if token == core.preview_token {
        return;
    }

    let (cap_w, cap_h) = core.preview_cap;
    match core
        .selected_candidate()
        .and_then(|candidate| candidate.preview.as_ref())
        .and_then(|preview| preview_image(preview, cap_w, cap_h))
    {
        Some(image) => {
            wizard.set_preview(image);
            wizard.set_has_preview(true);
        }
        None => wizard.set_has_preview(false),
    }
    core.preview_token = token;
}

fn sync_header_status(wizard: &Wizard, core: &WizardCore) {
    let i18n = &core.i18n;

    // header + status
    let header = core
        .selected_candidate()
        .and_then(|candidate| candidate.latency.map(|_| candidate.latency_text()))
        .map(|latency| {
            i18n.tr_with(
                "config.selector.header_with_latency",
                &[("latency", latency)],
            )
        })
        .unwrap_or_else(|| i18n.tr("config.selector.header"));
    wizard.set_header_text(header.into());

    let (status, is_error) = match core.selected_candidate() {
        None if core.candidates.is_empty() => (i18n.tr("config.selector.no_targets"), false),
        None => (i18n.tr("config.selector.no_selection"), false),
        Some(candidate) => {
            if let Some(error) = &candidate.error {
                (
                    i18n.tr_with(
                        "config.selector.selected_error",
                        &[("error", error.clone())],
                    ),
                    true,
                )
            } else if candidate.preview.is_none() {
                (i18n.tr("config.selector.error.waiting_probe"), false)
            } else {
                (
                    i18n.tr_with(
                        "config.selector.selected_ok",
                        &[
                            ("name", candidate.name.clone()),
                            ("latency", candidate.latency_text()),
                        ],
                    ),
                    false,
                )
            }
        }
    };
    wizard.set_status_text(status.into());
    wizard.set_status_error(is_error);
}

fn latency_class_index(class: LatencyClass) -> i32 {
    match class {
        LatencyClass::PaleGreen => 0,
        LatencyClass::Green => 1,
        LatencyClass::Yellow => 2,
        LatencyClass::Orange => 3,
        LatencyClass::Red => 4,
        LatencyClass::Unknown => 5,
    }
}

/// Cheap change-detector: rebuild the Slint row model only when a label,
/// latency, error, or the selection changed.
fn rows_signature(candidates: &[TargetCandidate], selected: Option<usize>) -> String {
    let mut sig = String::new();
    for (idx, candidate) in candidates.iter().enumerate() {
        sig.push_str(&candidate.fingerprint);
        sig.push('\u{1}');
        sig.push_str(&candidate.list_label());
        sig.push(if candidate.error.is_some() { 'E' } else { 'o' });
        sig.push(if selected == Some(idx) { '*' } else { '.' });
        sig.push('\u{2}');
    }
    sig
}

/// Structure-only signature (candidate identity + order). When this is
/// unchanged the row count and ordering match, so the model can be updated
/// cell-by-cell instead of reset.
fn rows_struct_signature(candidates: &[TargetCandidate]) -> String {
    let mut sig = String::new();
    for candidate in candidates {
        sig.push_str(&candidate.fingerprint);
        sig.push('\u{2}');
    }
    sig
}

/// Physical-pixel cap for the downscaled preview, derived from the window
/// scale and the fixed logical size of the preview pane in `wizard.slint`
/// (≈224×160). Computed once so the conversion knows its target size.
pub(super) fn preview_cap_for_scale(scale: f32) -> (u32, u32) {
    let cap_w = (224.0 * scale).ceil().max(1.0) as u32;
    let cap_h = (160.0 * scale).ceil().max(1.0) as u32;
    (cap_w, cap_h)
}

/// Build a Slint RGBA image from a captured (bottom-up) preview frame,
/// box-downscaled to fit within `cap_w`×`cap_h` physical pixels.
///
/// Downscaling here — at ≈4 Hz, when a probe frame arrives — is what keeps
/// the wizard responsive: the software renderer re-samples every `Image` on
/// each full-window repaint (and a hover animation repaints at 60+ Hz), so
/// handing it a full 720p/1080p game frame meant rescaling that frame dozens
/// of times a second. The pre-shrunk image makes each repaint cheap.
fn preview_image(frame: &PreviewFrame, cap_w: u32, cap_h: u32) -> Option<Image> {
    if frame.width == 0 || frame.height == 0 {
        return None;
    }
    let (target_w, target_h) = preview_target_size(frame.width, frame.height, cap_w, cap_h);
    let bytes = preview_rgba_scaled(frame, target_w, target_h)?;
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(target_w, target_h);
    let dst = buffer.make_mut_bytes();
    if dst.len() != bytes.len() {
        return None;
    }
    dst.copy_from_slice(&bytes);
    Some(Image::from_rgba8(buffer))
}

/// Largest size that fits within the cap while preserving aspect ratio.
/// Never upscales (a frame already smaller than the cap is kept 1:1).
fn preview_target_size(src_w: u32, src_h: u32, cap_w: u32, cap_h: u32) -> (u32, u32) {
    let cap_w = cap_w.max(1);
    let cap_h = cap_h.max(1);
    let scale = (f64::from(cap_w) / f64::from(src_w))
        .min(f64::from(cap_h) / f64::from(src_h))
        .min(1.0);
    let target_w = ((f64::from(src_w) * scale).round() as u32).max(1);
    let target_h = ((f64::from(src_h) * scale).round() as u32).max(1);
    (target_w, target_h)
}

/// Convert a captured frame (bottom-up, RGBA or BGR) into tightly-packed,
/// top-down RGBA8 bytes (`target_w * target_h * 4`), box-averaging each
/// destination pixel over its source block. With `target == source` this is
/// a straight flip + channel convert (one source pixel per destination).
fn preview_rgba_scaled(frame: &PreviewFrame, target_w: u32, target_h: u32) -> Option<Vec<u8>> {
    let bytes_per_pixel = match frame.format {
        PixelFormat::Rgba => 4usize,
        PixelFormat::Bgr => 3usize,
        PixelFormat::Bgra => 4usize,
    };
    let src_w = frame.width as usize;
    let src_h = frame.height as usize;
    let target_w = target_w as usize;
    let target_h = target_h as usize;
    if src_w == 0 || src_h == 0 || target_w == 0 || target_h == 0 {
        return None;
    }
    let source_stride = src_w.checked_mul(bytes_per_pixel)?;
    if frame.data.len() < source_stride.checked_mul(src_h)? {
        return None;
    }

    let mut output = vec![0u8; target_w * target_h * 4];
    for ty in 0..target_h {
        // Source rows (top-down) covered by this destination row.
        let sy0 = ty * src_h / target_h;
        let sy1 = (((ty + 1) * src_h / target_h).max(sy0 + 1)).min(src_h);
        for tx in 0..target_w {
            let sx0 = tx * src_w / target_w;
            let sx1 = (((tx + 1) * src_w / target_w).max(sx0 + 1)).min(src_w);
            let (mut r, mut g, mut b, mut count) = (0u32, 0u32, 0u32, 0u32);
            for sy_top in sy0..sy1 {
                let src_y = src_h - 1 - sy_top; // flip bottom-up -> top-down
                let row = src_y * source_stride;
                for sx in sx0..sx1 {
                    let src = row + sx * bytes_per_pixel;
                    match frame.format {
                        PixelFormat::Rgba => {
                            r += u32::from(frame.data[src]);
                            g += u32::from(frame.data[src + 1]);
                            b += u32::from(frame.data[src + 2]);
                        }
                        PixelFormat::Bgr | PixelFormat::Bgra => {
                            b += u32::from(frame.data[src]);
                            g += u32::from(frame.data[src + 1]);
                            r += u32::from(frame.data[src + 2]);
                        }
                    }
                    count += 1;
                }
            }
            let dst = (ty * target_w + tx) * 4;
            output[dst] = (r / count) as u8;
            output[dst + 1] = (g / count) as u8;
            output[dst + 2] = (b / count) as u8;
            output[dst + 3] = 255;
        }
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_rgba_bottom_up_keeps_channels_top_down() {
        let frame = PreviewFrame {
            width: 2,
            height: 2,
            format: PixelFormat::Rgba,
            data: vec![
                255, 0, 0, 255, 0, 255, 0, 255, // bottom row (src y0)
                0, 0, 255, 255, 255, 255, 255, 255, // top row (src y1)
            ],
        };
        // Output is top-down: first the source's last row, then its first.
        assert_eq!(
            preview_rgba_scaled(&frame, 2, 2).unwrap(),
            vec![
                0, 0, 255, 255, 255, 255, 255, 255, //
                255, 0, 0, 255, 0, 255, 0, 255,
            ]
        );
    }

    #[test]
    fn preview_bgr_bottom_up_converts_to_rgba_top_down() {
        let frame = PreviewFrame {
            width: 2,
            height: 2,
            format: PixelFormat::Bgr,
            data: vec![
                0, 0, 255, 0, 255, 0, // bottom row: blue, green (BGR)
                255, 0, 0, 255, 255, 255, // top row: red, white (BGR)
            ],
        };
        assert_eq!(
            preview_rgba_scaled(&frame, 2, 2).unwrap(),
            vec![
                0, 0, 255, 255, 255, 255, 255, 255, // blue, white (image top row)
                255, 0, 0, 255, 0, 255, 0, 255, // red, green (image bottom row)
            ]
        );
    }

    #[test]
    fn preview_box_downscale_averages_source_block() {
        // 2x2 grays, downscaled to a single pixel: the average of all four.
        let frame = PreviewFrame {
            width: 2,
            height: 2,
            format: PixelFormat::Rgba,
            data: vec![
                0, 0, 0, 255, 100, 100, 100, 255, // bottom row
                200, 200, 200, 255, 240, 240, 240, 255, // top row
            ],
        };
        // (0 + 100 + 200 + 240) / 4 == 135
        assert_eq!(
            preview_rgba_scaled(&frame, 1, 1).unwrap(),
            vec![135, 135, 135, 255]
        );
    }

    #[test]
    fn preview_target_size_preserves_aspect_and_never_upscales() {
        assert_eq!(preview_target_size(1920, 1080, 560, 400), (560, 315));
        assert_eq!(preview_target_size(1280, 720, 336, 240), (336, 189));
        // Source already smaller than the cap is kept 1:1.
        assert_eq!(preview_target_size(100, 100, 560, 400), (100, 100));
    }

    #[test]
    fn wizard_logical_height_tracks_debug_and_manual_panels() {
        // Collapsed debug panel: just the base height.
        assert_eq!(wizard_logical_height(false, false), WIZARD_LOGICAL_H);
        assert_eq!(wizard_logical_height(false, true), WIZARD_LOGICAL_H);
        // Expanded: recording-only vs the taller manual-target panel.
        assert_eq!(
            wizard_logical_height(true, false),
            WIZARD_LOGICAL_H + WIZARD_DEBUG_RECORDING_H
        );
        assert_eq!(
            wizard_logical_height(true, true),
            WIZARD_LOGICAL_H + WIZARD_DEBUG_MANUAL_H
        );
        assert!(
            wizard_logical_height(true, true) > wizard_logical_height(true, false),
            "the manual panel is taller than the recording-only panel"
        );
    }
}
