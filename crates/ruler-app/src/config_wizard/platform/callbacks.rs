//! Wizard UI wiring: one-time caption population, the Slint callback handlers
//! (target selection, browse buttons, refresh, confirm), the manual-target
//! config builder/validator, and the native Common Item Dialog file picker.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ruler_core::RulerConfig;
use slint::ComponentHandle;
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_APARTMENTTHREADED,
        },
        UI::{
            Shell::{
                Common::COMDLG_FILTERSPEC, FileOpenDialog, IFileOpenDialog, IShellItem,
                FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
            },
            WindowsAndMessaging::GetForegroundWindow,
        },
    },
};

use super::probe::{re_resolve_adb, refresh_candidates};
use super::WizardCore;
use crate::{i18n::I18n, slint_win::wide, ui::Wizard};

pub(super) fn populate_captions(
    wizard: &Wizard,
    i18n: &I18n,
    previous_config: Option<&RulerConfig>,
    debug: bool,
) {
    wizard.set_title_text(i18n.tr("config.window.title").into());
    wizard.set_header_text(i18n.tr("config.selector.header").into());
    wizard.set_status_text(i18n.tr("config.selector.scanning").into());
    wizard.set_status_error(false);
    wizard.set_has_preview(false);
    wizard.set_scanning(true);
    wizard.set_cap_preview(i18n.tr("config.selector.preview").into());
    wizard.set_cap_empty(i18n.tr("config.selector.scanning").into());
    wizard.set_cap_auto(i18n.tr("config.selector.auto_next").into());
    wizard.set_cap_refresh(i18n.tr("config.selector.refresh").into());
    wizard.set_cap_start(i18n.tr("config.btn.save_start").into());
    wizard.set_cap_cancel(i18n.tr("config.btn.cancel").into());
    wizard.set_preview_placeholder(i18n.tr("config.window.preview.unavailable").into());
    wizard.set_cap_telemetry(i18n.tr("config.selector.telemetry").into());
    wizard.set_cap_telemetry_hint(i18n.tr("config.selector.telemetry_hint").into());
    wizard.set_auto_checked(false);
    wizard.set_telemetry_checked(
        previous_config
            .and_then(|config| config.telemetry_enabled)
            .unwrap_or(true),
    );

    // adb-unavailable banner text. The flag itself is updated in
    // `sync_to_slint` based on `ruler_core::capture::adb_resolver::adb_available()`.
    wizard.set_adb_unavailable_message(i18n.tr("config.selector.adb_unavailable").into());
    wizard.set_adb_unavailable_hint(i18n.tr("config.selector.adb_unavailable_hint").into());

    // Debug panel state + captions
    wizard.set_debug_expanded(debug);
    wizard.set_cap_debug_header(i18n.tr("config.selector.debug_header").into());
    wizard.set_cap_debug_show(i18n.tr("config.selector.debug_show").into());
    wizard.set_cap_debug_hide(i18n.tr("config.selector.debug_hide").into());
    wizard.set_cap_debug_video(i18n.tr("config.selector.debug_video").into());
    wizard.set_cap_debug_csv(i18n.tr("config.selector.debug_csv").into());
    wizard.set_cap_debug_trace(i18n.tr("config.selector.debug_trace").into());
    wizard.set_cap_replay_path(i18n.tr("config.selector.replay_path").into());
    wizard.set_cap_replay_fps_label(i18n.tr("config.selector.replay_fps").into());

    // Manual target panel captions (replaces the old real/replay toggle).
    wizard.set_cap_manual_row(i18n.tr("config.selector.manual_row").into());
    wizard.set_cap_manual_header(i18n.tr("config.selector.manual_header").into());
    wizard.set_cap_manual_type(i18n.tr("config.selector.manual_type").into());
    wizard.set_cap_manual_type_mumu(i18n.tr("config.selector.manual_type_mumu").into());
    wizard.set_cap_manual_type_ldplayer(i18n.tr("config.selector.manual_type_ldplayer").into());
    wizard.set_cap_manual_type_adb(i18n.tr("config.selector.manual_type_adb").into());
    wizard.set_cap_manual_type_pc(i18n.tr("config.selector.manual_type_pc").into());
    wizard.set_cap_manual_type_virtual(i18n.tr("config.selector.manual_type_virtual").into());
    wizard.set_cap_manual_install(i18n.tr("config.selector.manual_install").into());
    wizard.set_cap_manual_instance(i18n.tr("config.selector.manual_instance").into());
    wizard.set_cap_manual_serial(i18n.tr("config.selector.manual_serial").into());
    wizard.set_cap_manual_window(i18n.tr("config.selector.manual_window").into());
    wizard.set_cap_manual_browse(i18n.tr("config.selector.manual_browse").into());

    wizard.set_record_video(
        previous_config
            .map(|config| config.debug_recording_video)
            .unwrap_or(false),
    );
    wizard.set_record_csv(
        previous_config
            .map(|config| config.debug_recording_csv)
            .unwrap_or(false),
    );
    wizard.set_trace_logging(
        previous_config
            .map(|config| config.trace_logging_enabled)
            .unwrap_or(false),
    );

    // Seed the manual fields + type from the previous config so re-opening
    // the wizard pre-fills whatever was last used.
    wizard.set_manual_kind(manual_kind_for(previous_config));
    wizard.set_manual_kind_open(false);
    wizard.set_manual_install_path(
        previous_config
            .and_then(|config| config.install_path.clone())
            .unwrap_or_default()
            .into(),
    );
    wizard.set_manual_instance_index(
        previous_config
            .and_then(|config| config.instance_index)
            .map(|index| index.to_string())
            .unwrap_or_default()
            .into(),
    );
    wizard.set_manual_device_id(
        previous_config
            .and_then(|config| config.device_id.clone())
            .unwrap_or_default()
            .into(),
    );
    wizard.set_manual_window_title(
        previous_config
            .and_then(|config| config.window_title.clone())
            .unwrap_or_default()
            .into(),
    );
    wizard.set_replay_path(
        previous_config
            .and_then(|config| config.replay_video_path.clone())
            .unwrap_or_default()
            .into(),
    );
    wizard.set_replay_fps_text(
        previous_config
            .and_then(|config| config.replay_fps)
            .map(|fps| fps.to_string())
            .unwrap_or_default()
            .into(),
    );
}

/// Map a previous config's capture type onto the manual dropdown index
/// (0 MuMu, 1 LDPlayer, 2 Adb, 3 Windows, 4 Replay).
fn manual_kind_for(previous_config: Option<&RulerConfig>) -> i32 {
    match previous_config.map(|config| config.capture_type.as_str()) {
        Some("ldplayer") => 1,
        Some("adb" | "minicap") => 2,
        Some("window") => 3,
        Some("replay") => 4,
        _ => 0,
    }
}

/// A deferred browse action. Native file dialogs run their own modal message
/// loop, which re-enters the wizard's `WM_TIMER`/`tick`; opening one from
/// inside a Slint callback would alias the `&mut WizardWindow` that the
/// pointer handler still holds. So the callback only *stages* the action here
/// and the top-level message loop runs it once that borrow is released.
pub(super) type BrowseAction = Box<dyn FnOnce()>;

pub(super) fn wire_callbacks(
    wizard: &Wizard,
    core: &Rc<RefCell<WizardCore>>,
    result: &Rc<RefCell<Option<RulerConfig>>>,
    closing: &Rc<Cell<bool>>,
    drag_on_title: &Rc<Cell<bool>>,
    pending_browse: &Rc<Cell<Option<BrowseAction>>>,
) {
    wizard.on_select_target({
        let core = Rc::clone(core);
        move |idx| {
            let mut core = core.borrow_mut();
            // Picking a discovered candidate leaves manual mode; the manual
            // panel / dropdown are reconciled to hidden by `sync_to_slint`.
            core.manual_mode = false;
            core.manual_error = None;
            // Force the preview to re-push: it was suppressed while manual.
            core.preview_token = None;
            core.selected_index = (idx >= 0).then_some(idx as usize);
        }
    });
    wizard.on_select_manual({
        let core = Rc::clone(core);
        move || {
            let mut core = core.borrow_mut();
            core.manual_mode = true;
            core.manual_error = None;
            core.preview_token = None;
            core.selected_index = None;
        }
    });
    wizard.on_browse_install({
        let pending = Rc::clone(pending_browse);
        let weak = wizard.as_weak();
        move || {
            let weak = weak.clone();
            pending.set(Some(Box::new(move || {
                if let Some(path) = pick_path(PickKind::Folder) {
                    if let Some(wizard) = weak.upgrade() {
                        wizard.set_manual_install_path(path.into());
                    }
                }
            })));
        }
    });
    wizard.on_browse_replay({
        let pending = Rc::clone(pending_browse);
        let core = Rc::clone(core);
        let weak = wizard.as_weak();
        move || {
            let weak = weak.clone();
            let core = Rc::clone(&core);
            pending.set(Some(Box::new(move || {
                let filter_name = core.borrow().i18n.tr("config.selector.manual_filter_video");
                if let Some(path) = pick_path(PickKind::File {
                    filter_name: &filter_name,
                    filter_spec: "*.mkv;*.mp4;*.mov;*.avi;*.webm;*.flv;*.ts;*.m4v",
                }) {
                    if let Some(wizard) = weak.upgrade() {
                        wizard.set_replay_path(path.into());
                    }
                }
            })));
        }
    });
    wizard.on_refresh({
        let core = Rc::clone(core);
        move || {
            // The user likely clicked refresh because they started an
            // emulator or installed platform-tools since opening the
            // wizard — re-resolve adb so the cache reflects the new state
            // before discover_targets() runs.
            re_resolve_adb();
            refresh_candidates(&mut core.borrow_mut())
        }
    });
    wizard.on_cancel({
        let closing = Rc::clone(closing);
        move || closing.set(true)
    });
    wizard.on_close({
        let closing = Rc::clone(closing);
        move || closing.set(true)
    });
    wizard.on_title_pressed({
        let drag_on_title = Rc::clone(drag_on_title);
        move || drag_on_title.set(true)
    });
    wizard.on_title_released({
        let drag_on_title = Rc::clone(drag_on_title);
        move || drag_on_title.set(false)
    });
    wizard.on_open_update({
        let core = Rc::clone(core);
        move || {
            let url = core
                .borrow()
                .app_state
                .snapshot()
                .ui
                .update_notice
                .map(|notice| notice.html_url);
            if let Some(url) = url {
                unsafe { crate::menu::win32::open_url(&url) };
            }
        }
    });
    wizard.on_confirm({
        let core = Rc::clone(core);
        let result = Rc::clone(result);
        let closing = Rc::clone(closing);
        let weak = wizard.as_weak();
        move || {
            let wizard = weak.upgrade().expect("wizard dropped");
            let auto = wizard.get_auto_checked();
            let telemetry_enabled = wizard.get_telemetry_checked();
            let record_video = wizard.get_record_video();
            let record_csv = wizard.get_record_csv();
            let trace_logging = wizard.get_trace_logging();

            let config = if core.borrow().manual_mode {
                // Manual target: build the config from the typed fields,
                // validating the required ones for the chosen type.
                let draft = ManualDraft {
                    kind: wizard.get_manual_kind(),
                    install_path: wizard.get_manual_install_path().to_string(),
                    instance_index: wizard.get_manual_instance_index().to_string(),
                    device_id: wizard.get_manual_device_id().to_string(),
                    window_title: wizard.get_manual_window_title().to_string(),
                    replay_path: wizard.get_replay_path().to_string(),
                    replay_fps_text: wizard.get_replay_fps_text().to_string(),
                    auto,
                    record_video,
                    record_csv,
                    trace_logging,
                    telemetry_enabled,
                };
                let core_ref = core.borrow();
                let built = build_manual_config(
                    &draft,
                    core_ref.previous_config.as_ref(),
                    core_ref.i18n.locale(),
                    &core_ref.i18n.tr("config.selector.manual_invalid"),
                );
                drop(core_ref);
                match built {
                    Ok(config) => config,
                    Err(error) => {
                        core.borrow_mut().manual_error = Some(error);
                        return;
                    }
                }
            } else {
                let core = core.borrow();
                let Some(candidate) = core.selected_candidate() else {
                    return;
                };
                if candidate.error.is_some() || candidate.preview.is_none() {
                    return;
                }
                let mut config = candidate.config.clone();
                config.language = core
                    .previous_config
                    .as_ref()
                    .and_then(|previous| previous.language.clone())
                    .or_else(|| Some(core.i18n.locale().to_string()));
                config.auto_select_target = auto;
                config.target_fingerprint = Some(candidate.fingerprint.clone());
                config.telemetry_enabled = Some(telemetry_enabled);
                config.screenshot_delay_ms = candidate
                    .latency
                    .map(|latency| latency.as_secs_f64() * 1000.0);
                config.debug_recording_enabled = record_video || record_csv;
                config.debug_recording_video = record_video;
                config.debug_recording_csv = record_csv;
                config.trace_logging_enabled = trace_logging;
                config
            };
            *result.borrow_mut() = Some(config);
            closing.set(true);
        }
    });
}

/// Typed manual-target inputs read from the Slint properties on confirm.
struct ManualDraft {
    kind: i32,
    install_path: String,
    instance_index: String,
    device_id: String,
    window_title: String,
    replay_path: String,
    replay_fps_text: String,
    auto: bool,
    record_video: bool,
    record_csv: bool,
    trace_logging: bool,
    telemetry_enabled: bool,
}

/// Build a `RulerConfig` from a manual draft, validating the fields the
/// chosen capture type requires. Shared fields (calibration, overlay,
/// language, log dir, ...) are seeded from `previous`, mirroring
/// `target_discovery::base_config`. Returns `invalid_msg` when a required
/// field is empty.
fn build_manual_config(
    draft: &ManualDraft,
    previous: Option<&RulerConfig>,
    locale: &str,
    invalid_msg: &str,
) -> Result<RulerConfig, String> {
    let non_empty = |value: &str| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    };
    let install_path = non_empty(&draft.install_path);
    let instance_index = draft.instance_index.trim().parse::<u32>().ok();
    let device_id = non_empty(&draft.device_id);
    let window_title = non_empty(&draft.window_title);
    let replay_path = non_empty(&draft.replay_path);

    let capture_type = match draft.kind {
        0 => "mumu",
        1 => "ldplayer",
        2 => "adb",
        3 => "window",
        4 => "replay",
        _ => return Err(invalid_msg.to_string()),
    };
    // Required-field validation per type.
    match draft.kind {
        0 | 1 if install_path.is_none() => return Err(invalid_msg.to_string()),
        2 if device_id.is_none() => return Err(invalid_msg.to_string()),
        3 if window_title.is_none() => return Err(invalid_msg.to_string()),
        4 if replay_path.is_none() => return Err(invalid_msg.to_string()),
        _ => {}
    }

    let is_emulator = draft.kind == 0 || draft.kind == 1;
    let fingerprint = match draft.kind {
        0 | 1 => format!(
            "manual:{capture_type}:{}:{}",
            install_path.as_deref().unwrap_or_default(),
            instance_index.unwrap_or(0)
        ),
        2 => format!("manual:adb:{}", device_id.as_deref().unwrap_or_default()),
        3 => format!(
            "manual:window:{}",
            window_title.as_deref().unwrap_or_default()
        ),
        _ => format!(
            "manual:replay:{}",
            replay_path.as_deref().unwrap_or_default()
        ),
    };
    let replay_fps =
        (draft.kind == 4).then(|| draft.replay_fps_text.trim().parse::<f64>().unwrap_or(60.0));

    Ok(RulerConfig {
        uuid: previous.and_then(|config| config.uuid.clone()),
        telemetry_enabled: Some(draft.telemetry_enabled),
        screenshot_delay_ms: None,
        capture_type: capture_type.to_string(),
        install_path: is_emulator.then_some(install_path).flatten(),
        instance_index: is_emulator.then_some(instance_index).flatten(),
        device_id: matches!(draft.kind, 0..=2).then_some(device_id).flatten(),
        window_handle: None,
        window_title: (draft.kind == 3).then_some(window_title).flatten(),
        window_class: None,
        active_calibration_profile: previous
            .and_then(|config| config.active_calibration_profile.clone()),
        frame_display_mode: previous
            .and_then(|config| config.frame_display_mode.clone())
            .or_else(|| Some("0_to_n-1".to_string())),
        language: previous
            .and_then(|config| config.language.clone())
            .or_else(|| Some(locale.to_string())),
        auto_select_target: draft.auto,
        target_fingerprint: Some(fingerprint),
        overlay_pos_x: previous.and_then(|config| config.overlay_pos_x),
        overlay_pos_y: previous.and_then(|config| config.overlay_pos_y),
        overlay_scale: previous.and_then(|config| config.overlay_scale),
        ui_scaler: previous.and_then(|config| config.ui_scaler),
        debug_recording_enabled: draft.record_video || draft.record_csv,
        debug_recording_video: draft.record_video,
        debug_recording_csv: draft.record_csv,
        trace_logging_enabled: draft.trace_logging,
        log_output_dir: previous.and_then(|config| config.log_output_dir.clone()),
        replay_video_path: (draft.kind == 4).then_some(replay_path).flatten(),
        replay_fps,
    })
}

/// What a browse button should pick.
enum PickKind<'a> {
    /// A folder — the emulator install directory.
    Folder,
    /// A single existing file, restricted to one filter (name, spec).
    File {
        filter_name: &'a str,
        filter_spec: &'a str,
    },
}

/// Show a native Common Item Dialog and return the chosen filesystem path, or
/// `None` if the user cancelled or the dialog could not be created. The dialog
/// is owned by the active wizard window so it surfaces above the topmost
/// layered window. Uses `IFileOpenDialog` directly (the `windows` crate is
/// already a dependency); no third-party file-dialog crate is pulled in.
fn pick_path(kind: PickKind) -> Option<String> {
    unsafe {
        // The wizard thread is not otherwise COM-initialized; bring up an STA
        // apartment for the dialog and tear it back down when we own the init.
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let picked = pick_path_inner(kind);
        if init.is_ok() {
            CoUninitialize();
        }
        picked
    }
}

unsafe fn pick_path_inner(kind: PickKind) -> Option<String> {
    let dialog: IFileOpenDialog =
        CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
    let mut options = dialog.GetOptions().ok()?;
    options |= FOS_FORCEFILESYSTEM;
    // `name`/`spec` must outlive `SetFileTypes`; keep them in this scope.
    let (name, spec);
    match kind {
        PickKind::Folder => options |= FOS_PICKFOLDERS,
        PickKind::File {
            filter_name,
            filter_spec,
        } => {
            options |= FOS_FILEMUSTEXIST;
            name = wide(filter_name);
            spec = wide(filter_spec);
            let filters = [COMDLG_FILTERSPEC {
                pszName: PCWSTR(name.as_ptr()),
                pszSpec: PCWSTR(spec.as_ptr()),
            }];
            dialog.SetFileTypes(&filters).ok()?;
        }
    }
    dialog.SetOptions(options).ok()?;
    // `Show` returns Err when the user cancels.
    dialog.Show(GetForegroundWindow()).ok()?;
    let item: IShellItem = dialog.GetResult().ok()?;
    let raw: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
    let path = raw.to_string().ok();
    CoTaskMemFree(Some(raw.0 as *const std::ffi::c_void));
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manual_draft(kind: i32) -> ManualDraft {
        ManualDraft {
            kind,
            telemetry_enabled: true,
            install_path: String::new(),
            instance_index: String::new(),
            device_id: String::new(),
            window_title: String::new(),
            replay_path: String::new(),
            replay_fps_text: String::new(),
            auto: false,
            record_video: false,
            record_csv: false,
            trace_logging: false,
        }
    }

    #[test]
    fn build_manual_config_validates_required_fields() {
        // MuMu needs an install path; empty -> the invalid message.
        let draft = manual_draft(0);
        assert_eq!(
            build_manual_config(&draft, None, "zh_CN", "missing").unwrap_err(),
            "missing"
        );
        // Generic ADB needs a serial.
        assert_eq!(
            build_manual_config(&manual_draft(2), None, "zh_CN", "missing").unwrap_err(),
            "missing"
        );
        // An out-of-range kind is rejected too.
        assert_eq!(
            build_manual_config(&manual_draft(9), None, "zh_CN", "missing").unwrap_err(),
            "missing"
        );
    }

    #[test]
    fn build_manual_config_maps_virtual_and_adb_fields() {
        // Virtual: video path required, fps parsed (default 60 when blank).
        let mut draft = manual_draft(4);
        draft.replay_path = "  C:/clip.mkv  ".to_string();
        let config = build_manual_config(&draft, None, "en_US", "missing").unwrap();
        assert_eq!(config.capture_type, "replay");
        assert_eq!(config.replay_video_path.as_deref(), Some("C:/clip.mkv"));
        assert_eq!(config.replay_fps, Some(60.0));
        assert_eq!(config.language.as_deref(), Some("en_US"));

        // Generic ADB: serial maps onto device_id, no install/window fields.
        let mut adb = manual_draft(2);
        adb.device_id = "127.0.0.1:16384".to_string();
        let config = build_manual_config(&adb, None, "zh_CN", "missing").unwrap();
        assert_eq!(config.capture_type, "adb");
        assert_eq!(config.device_id.as_deref(), Some("127.0.0.1:16384"));
        assert!(config.install_path.is_none());
        assert!(config.window_title.is_none());

        // MuMu: install path + instance index, blank index defaults later to 0.
        let mut mumu = manual_draft(0);
        mumu.install_path = "D:/MuMu".to_string();
        mumu.instance_index = "2".to_string();
        let config = build_manual_config(&mumu, None, "zh_CN", "missing").unwrap();
        assert_eq!(config.capture_type, "mumu");
        assert_eq!(config.install_path.as_deref(), Some("D:/MuMu"));
        assert_eq!(config.instance_index, Some(2));
    }
}
