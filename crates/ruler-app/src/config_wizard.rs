use ruler_core::RulerConfig;

use crate::{i18n::I18n, resources::ResourceLocator};

pub fn run_config_wizard(
    _resources: &ResourceLocator,
    i18n: &I18n,
    previous_config: Option<&RulerConfig>,
    debug: bool,
) -> Option<RulerConfig> {
    platform::run_config_wizard(i18n, previous_config, debug)
}

#[cfg(not(windows))]
mod platform {
    use ruler_core::RulerConfig;

    use crate::i18n::I18n;

    pub fn run_config_wizard(
        _: &I18n,
        _: Option<&RulerConfig>,
        _debug: bool,
    ) -> Option<RulerConfig> {
        None
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        cell::{Cell, RefCell},
        collections::{HashMap, VecDeque},
        rc::Rc,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc::{self, Receiver, Sender},
            Arc,
        },
        thread::{self, JoinHandle},
        time::Duration,
    };

    use ruler_core::{
        capture::{create_backend, CapturedFrame},
        PixelFormat, RulerConfig,
    };
    use slint::{
        platform::{
            software_renderer::{MinimalSoftwareWindow, SoftwareRenderer},
            Key, PointerEventButton, WindowAdapter, WindowEvent,
        },
        ComponentHandle, Image, Model, ModelRc, PhysicalSize, Rgba8Pixel, SharedPixelBuffer,
        SharedString, VecModel,
    };
    use windows::{
        core::{PCWSTR, PWSTR},
        Win32::{
            Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
            Graphics::Gdi::{DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ},
            System::{
                Com::{
                    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
                    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
                },
                LibraryLoader::GetModuleHandleW,
            },
            UI::{
                Input::KeyboardAndMouse::{
                    ReleaseCapture, SetCapture, VK_BACK, VK_DELETE, VK_END, VK_ESCAPE, VK_HOME,
                    VK_LEFT, VK_RETURN, VK_RIGHT,
                },
                Shell::{
                    Common::COMDLG_FILTERSPEC, FileOpenDialog, IFileOpenDialog, IShellItem,
                    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
                },
                WindowsAndMessaging::{
                    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                    GetCursorPos, GetForegroundWindow, GetMessageW, GetSystemMetrics,
                    GetWindowLongPtrW, GetWindowRect, LoadCursorW, RegisterClassW,
                    SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
                    TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HMENU,
                    IDC_ARROW, MSG, SM_CXSCREEN, SM_CYSCREEN, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
                    SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR, WM_DESTROY, WM_KEYDOWN,
                    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE,
                    WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
                },
            },
        },
    };

    use crate::{
        i18n::I18n,
        slint_win::{create_dib, ensure_platform, logical_pos, present_layered, wide, PreBgra},
        target_discovery::{
            discover_targets, latency_class, LatencyClass, PreviewFrame, TargetCandidate,
        },
        ui::{TargetRow, Wizard},
    };

    const CLASS_NAME: &str = "RulerWizardWindowClass";
    const TIMER_ID: usize = 1;
    const TICK_INTERVAL_MS: u32 = 16;
    const PROBE_LOOP_PAUSE_MS: u64 = 250;
    const PROBE_RECONNECT_PAUSE_MS: u64 = 1000;
    const LATENCY_SAMPLE_WINDOW: usize = 12;
    // Mouse-wheel scrolling for the target list. One wheel notch is
    // `WHEEL_DELTA` (120) raw units; map each notch to `WHEEL_STEP_LOGICAL_PX`
    // logical pixels of Flickable travel, mirroring Slint's own backends
    // (~60 logical px per line) so the list scrolls at a familiar speed.
    const WHEEL_DELTA_UNIT: f32 = 120.0;
    const WHEEL_STEP_LOGICAL_PX: f32 = 60.0;
    // Fixed logical design size of `wizard.slint` (physical = logical * scale).
    const WIZARD_LOGICAL_W: f32 = 560.0;
    const WIZARD_LOGICAL_H: f32 = 404.0;
    // Extra logical height for the debug panel. Two budgets: the recording-only
    // section (manual target off) and the taller recording + manual-target panel
    // (sized for the dropdown-open type list / the 3-field MuMu-LDPlayer case).
    const WIZARD_DEBUG_RECORDING_H: f32 = 150.0;
    const WIZARD_DEBUG_MANUAL_H: f32 = 300.0;

    #[derive(Clone, Debug)]
    struct ProbeMessage {
        generation: u64,
        fingerprint: String,
        result: Result<(Duration, PreviewFrame), String>,
    }

    struct ProbeWorker {
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    /// All mutable wizard data shared between the Slint callbacks and the render
    /// tick. Both run on the single UI thread, so a plain `Rc<RefCell<_>>` is safe.
    struct WizardCore {
        i18n: I18n,
        previous_config: Option<RulerConfig>,
        candidates: Vec<TargetCandidate>,
        selected_index: Option<usize>,
        // When the trailing "manual" list row is active the candidate path is
        // bypassed: the target is built from typed fields (held in the Slint
        // in-out properties) on confirm. `selected_index` is `None` while this
        // is set, so no candidate is highlighted or probed for the selection.
        manual_mode: bool,
        // Set when a manual confirm fails validation; rendered on the status line
        // until the selection or fields change.
        manual_error: Option<String>,
        probe_tx: Sender<ProbeMessage>,
        probe_rx: Receiver<ProbeMessage>,
        probe_generation: u64,
        probe_workers: Vec<ProbeWorker>,
        latency_samples: HashMap<String, VecDeque<Duration>>,
        // Persistent target-list model, updated in place between structural
        // changes (see `sync_rows`) so latency ticks don't tear down the
        // repeater and reset its row hover animations.
        rows_model: Rc<VecModel<TargetRow>>,
        // `None` until the first sync; reset to `None` on refresh to force a
        // re-evaluation. Holds the last pushed content signature otherwise.
        rows_content_sig: Option<String>,
        rows_struct_sig: String,
        // Physical pixel cap for the downscaled preview image (see `preview_image`).
        preview_cap: (u32, u32),
        // identity of the preview currently pushed to Slint: (selected idx, data ptr, len)
        preview_token: Option<(usize, usize, usize)>,
    }

    impl WizardCore {
        fn selected_candidate(&self) -> Option<&TargetCandidate> {
            self.selected_index
                .and_then(|index| self.candidates.get(index))
        }

        fn selected_fingerprint(&self) -> Option<String> {
            self.selected_candidate()
                .map(|candidate| candidate.fingerprint.clone())
        }
    }

    impl Drop for WizardCore {
        fn drop(&mut self) {
            stop_probe_worker_list(&mut self.probe_workers);
        }
    }

    /// The HWND-bound state: the Slint component, its software framebuffer, and a
    /// handle to the shared [`WizardCore`].
    struct WizardWindow {
        wizard: Wizard,
        window: Rc<MinimalSoftwareWindow>,
        core: Rc<RefCell<WizardCore>>,
        closing: Rc<Cell<bool>>,
        drag_on_title: Rc<Cell<bool>>,
        // Currently applied logical window height; the render tick recomputes the
        // target height (debug panel + manual panel toggles) and resizes on change.
        logical_h: f32,
        // Buffers the high half of a UTF-16 surrogate pair across WM_CHAR messages
        // while forwarding text input to the focused Slint TextInput.
        pending_high: Cell<u16>,
        mem_dc: HDC,
        dib: HBITMAP,
        bits: *mut PreBgra,
        buf_w: usize,
        buf_h: usize,
        scale: f32,
        mouse_down: bool,
        moved: bool,
        drag_origin: POINT,
        window_origin: POINT,
    }

    impl Drop for WizardWindow {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteDC(self.mem_dc);
                let _ = DeleteObject(HGDIOBJ(self.dib.0));
            }
        }
    }

    pub fn run_config_wizard(
        i18n: &I18n,
        previous_config: Option<&RulerConfig>,
        debug: bool,
    ) -> Option<RulerConfig> {
        let slot = ensure_platform();
        let Ok(wizard) = Wizard::new() else {
            return None;
        };
        let window = slot.borrow_mut().take()?;

        // Computed once up front: drives both the native window size and the
        // physical pixel cap for the downscaled live preview.
        let scale = wizard_scale();

        // Persistent row model. The render tick updates it in place (a latency
        // tick changes only a couple of cells) instead of replacing it, so the
        // target list's repeater is not torn down — and its hover animations
        // not reset — several times a second.
        let rows_model: Rc<VecModel<TargetRow>> = Rc::new(VecModel::default());
        wizard.set_rows(ModelRc::from(rows_model.clone()));

        let (probe_tx, probe_rx) = mpsc::channel();
        let core = Rc::new(RefCell::new(WizardCore {
            i18n: i18n.clone(),
            previous_config: previous_config.cloned(),
            candidates: Vec::new(),
            selected_index: None,
            manual_mode: false,
            manual_error: None,
            probe_tx,
            probe_rx,
            probe_generation: 0,
            probe_workers: Vec::new(),
            latency_samples: HashMap::new(),
            rows_model,
            rows_content_sig: None,
            rows_struct_sig: String::new(),
            preview_cap: preview_cap_for_scale(scale),
            preview_token: None,
        }));
        let result: Rc<RefCell<Option<RulerConfig>>> = Rc::new(RefCell::new(None));
        let closing = Rc::new(Cell::new(false));
        let drag_on_title = Rc::new(Cell::new(false));
        // Browse buttons stage their file dialog here; the message loop runs it
        // outside the pointer handler's `&mut WizardWindow` borrow (see BrowseAction).
        let pending_browse: Rc<Cell<Option<BrowseAction>>> = Rc::new(Cell::new(None));

        populate_captions(&wizard, i18n, previous_config, debug);
        wire_callbacks(
            &wizard,
            &core,
            &result,
            &closing,
            &drag_on_title,
            &pending_browse,
        );

        // Resolve adb before the first discovery pass: the resolver cache is
        // consulted by every adb call site (AdbController, LDPlayerController,
        // AndroidInputOverlayGuard, target_discovery). Re-resolving here also
        // picks up any emulator started since `main.rs` did its initial probe.
        re_resolve_adb();

        // Initial discovery + probe.
        refresh_candidates(&mut core.borrow_mut());

        let width = (WIZARD_LOGICAL_W * scale).round() as i32;
        let logical_h = wizard_logical_height(debug, false);
        let height = (logical_h * scale).round() as i32;
        let _ = window
            .window()
            .try_dispatch_event(WindowEvent::ScaleFactorChanged {
                scale_factor: scale,
            });
        window.set_size(PhysicalSize::new(width as u32, height as u32));
        let _ = wizard.show();
        let _ = window
            .window()
            .try_dispatch_event(WindowEvent::WindowActiveChanged(true));

        unsafe {
            let Ok(module) = GetModuleHandleW(PCWSTR::null()) else {
                return None;
            };
            let class_name = wide(CLASS_NAME);
            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                hInstance: HINSTANCE(module.0),
                lpszClassName: PCWSTR(class_name.as_ptr()),
                hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            };
            RegisterClassW(&class);

            let screen_w = GetSystemMetrics(SM_CXSCREEN);
            let screen_h = GetSystemMetrics(SM_CYSCREEN);
            let left = ((screen_w - width) / 2).max(0);
            let top = ((screen_h - height) / 2).max(0);

            let (mem_dc, dib, bits) = create_dib(width, height)?;

            let state = Box::new(WizardWindow {
                wizard,
                window,
                core,
                closing: Rc::clone(&closing),
                drag_on_title,
                logical_h,
                pending_high: Cell::new(0),
                mem_dc,
                dib,
                bits,
                buf_w: width as usize,
                buf_h: height as usize,
                scale,
                mouse_down: false,
                moved: false,
                drag_origin: POINT::default(),
                window_origin: POINT::default(),
            });
            let state_ptr = Box::into_raw(state);

            let title = wide("Arknights Ruler Setup");
            let Ok(hwnd) = CreateWindowExW(
                WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_LAYERED.0),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WINDOW_STYLE(WS_POPUP.0 | WS_VISIBLE.0),
                left,
                top,
                width,
                height,
                HWND::default(),
                HMENU::default(),
                HINSTANCE(module.0),
                Some(state_ptr.cast()),
            ) else {
                let _ = Box::from_raw(state_ptr);
                return None;
            };
            if hwnd.0.is_null() {
                let _ = Box::from_raw(state_ptr);
                return None;
            }

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetTimer(hwnd, TIMER_ID, TICK_INTERVAL_MS, None);
            tick(hwnd);

            let mut message = MSG::default();
            while !closing.get() {
                let value = GetMessageW(&mut message, HWND::default(), 0, 0).0;
                if value == 0 || value == -1 {
                    break;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
                // Run any browse dialog staged by a button this iteration. We are
                // back at the top level here, so no `&mut WizardWindow` is held
                // while the dialog pumps its own (re-entrant) message loop.
                if let Some(action) = pending_browse.take() {
                    action();
                }
            }

            let _ = DestroyWindow(hwnd);
        }

        let config = result.borrow_mut().take();
        config
    }

    fn populate_captions(
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
        wizard.set_auto_checked(false);

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
        wizard
            .set_cap_manual_type_ldplayer(i18n.tr("config.selector.manual_type_ldplayer").into());
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
                .and_then(|config| config.replay_hevc_path.clone())
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
    type BrowseAction = Box<dyn FnOnce()>;

    fn wire_callbacks(
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
        wizard.on_confirm({
            let core = Rc::clone(core);
            let result = Rc::clone(result);
            let closing = Rc::clone(closing);
            let weak = wizard.as_weak();
            move || {
                let wizard = weak.upgrade().expect("wizard dropped");
                let auto = wizard.get_auto_checked();
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
            3 => format!("manual:window:{}", window_title.as_deref().unwrap_or_default()),
            _ => format!("manual:replay:{}", replay_path.as_deref().unwrap_or_default()),
        };
        let replay_fps = (draft.kind == 4)
            .then(|| draft.replay_fps_text.trim().parse::<f64>().unwrap_or(60.0));

        Ok(RulerConfig {
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
            replay_hevc_path: (draft.kind == 4).then_some(replay_path).flatten(),
            replay_fps,
        })
    }

    // ===================== render / sync tick =====================

    unsafe fn tick(hwnd: HWND) {
        slint::platform::update_timers_and_animations();
        let Some(state) = wizard_window_mut(hwnd) else {
            return;
        };
        {
            let mut core = state.core.borrow_mut();
            drain_probe_messages(&mut core);
            sync_to_slint(&state.wizard, &mut core);
        }
        let debug_expanded = state.wizard.get_debug_expanded();
        let manual_mode = state.core.borrow().manual_mode;
        let want_h = wizard_logical_height(debug_expanded, manual_mode);
        if (want_h - state.logical_h).abs() > 0.5 {
            resize_wizard(hwnd, state, want_h);
        }

        let w = state.buf_w;
        let h = state.buf_h;
        let bits = state.bits;
        let drawn = state.window.draw_if_needed(|renderer: &SoftwareRenderer| {
            let buffer = unsafe { std::slice::from_raw_parts_mut(bits, w * h) };
            renderer.render(buffer, w);
        });
        if drawn {
            present_layered(hwnd, state.mem_dc, w as i32, h as i32);
        }
    }

    fn wizard_logical_height(debug_expanded: bool, manual_mode: bool) -> f32 {
        if !debug_expanded {
            WIZARD_LOGICAL_H
        } else if manual_mode {
            WIZARD_LOGICAL_H + WIZARD_DEBUG_MANUAL_H
        } else {
            WIZARD_LOGICAL_H + WIZARD_DEBUG_RECORDING_H
        }
    }

    unsafe fn resize_wizard(hwnd: HWND, state: &mut WizardWindow, logical_h: f32) {
        let width = state.buf_w as i32;
        let height = (logical_h * state.scale).round() as i32;
        let _ = DeleteDC(state.mem_dc);
        let _ = DeleteObject(HGDIOBJ(state.dib.0));
        if let Some((mem_dc, dib, bits)) = create_dib(width, height) {
            state.mem_dc = mem_dc;
            state.dib = dib;
            state.bits = bits;
            state.buf_h = height as usize;
        }
        state.logical_h = logical_h;
        state
            .window
            .set_size(PhysicalSize::new(width as u32, height as u32));
        let _ = SetWindowPos(
            hwnd,
            HWND::default(),
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER,
        );
    }

    /// Push the current candidate state into the Slint component. Each section
    /// is change-gated so the render tick stays cheap: rows update in place, the
    /// preview is rebuilt only when a new frame arrives for the selection, and
    /// the header/status assignments rely on Slint's own equality check.
    fn sync_to_slint(wizard: &Wizard, core: &mut WizardCore) {
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

    /// Re-probe `adb` on `PATH` and any emulator-bundled `adb.exe` from
    /// currently running MuMu / LDPlayer processes. Updates the process-global
    /// resolver cache. Cheap (one `adb version` call per candidate) and safe
    /// to call repeatedly.
    fn re_resolve_adb() {
        let candidates = crate::target_discovery::discover_emulator_adb_paths();
        match ruler_core::capture::adb_resolver::resolve_adb_with(&candidates) {
            Some(exe) => log::info!(
                "adb resolved for wizard: {} (from_path={})",
                exe.path(),
                exe.from_path()
            ),
            None => log::warn!("adb could not be resolved; wizard will show adb-unavailable banner"),
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

    // ===================== discovery + probe workers =====================

    fn refresh_candidates(core: &mut WizardCore) {
        let preferred_fingerprint = core.selected_fingerprint().or_else(|| {
            core.previous_config
                .as_ref()
                .and_then(|config| config.target_fingerprint.clone())
        });
        let previous_probe_state = core
            .candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.fingerprint.clone(),
                    (
                        candidate.latency,
                        candidate.latency_class,
                        candidate.preview.clone(),
                        candidate.error.clone(),
                    ),
                )
            })
            .collect::<HashMap<_, _>>();

        stop_probe_worker_list(&mut core.probe_workers);
        core.probe_generation = core.probe_generation.wrapping_add(1);
        core.preview_token = None;
        // Force the next sync to re-evaluate the rows. The structure signature is
        // left intact so an unchanged candidate set updates in place rather than
        // tearing down the repeater.
        core.rows_content_sig = None;

        core.candidates = discover_targets(core.previous_config.as_ref());
        for candidate in &mut core.candidates {
            if let Some((latency, latency_class, preview, error)) =
                previous_probe_state.get(&candidate.fingerprint)
            {
                candidate.latency = *latency;
                candidate.latency_class = *latency_class;
                candidate.preview = preview.clone();
                candidate.error = error.clone();
            }
        }
        let active = core
            .candidates
            .iter()
            .map(|candidate| candidate.fingerprint.clone())
            .collect::<std::collections::HashSet<_>>();
        core.latency_samples
            .retain(|fingerprint, _| active.contains(fingerprint));

        if core.candidates.is_empty() {
            core.selected_index = None;
            return;
        }
        let selected =
            preferred_selection_index(&core.candidates, preferred_fingerprint.as_deref())
                .unwrap_or(0);
        // A refresh re-discovers candidates but must not steal the selection from
        // an active manual target (the manual row stays selected, no candidate is).
        core.selected_index = (!core.manual_mode).then_some(selected);
        start_probe_workers(core);
    }

    fn start_probe_workers(core: &mut WizardCore) {
        let probes = core
            .candidates
            .iter()
            .map(|candidate| (candidate.fingerprint.clone(), candidate.config.clone()))
            .collect::<Vec<_>>();
        for (fingerprint, config) in probes {
            let stop = Arc::new(AtomicBool::new(false));
            match spawn_probe_worker(
                fingerprint.clone(),
                config,
                core.probe_generation,
                core.probe_tx.clone(),
                Arc::clone(&stop),
            ) {
                Ok(handle) => core.probe_workers.push(ProbeWorker {
                    stop,
                    handle: Some(handle),
                }),
                Err(error) => {
                    send_probe_error(&core.probe_tx, core.probe_generation, &fingerprint, error)
                }
            }
        }
    }

    fn drain_probe_messages(core: &mut WizardCore) {
        while let Ok(message) = core.probe_rx.try_recv() {
            if message.generation != core.probe_generation {
                continue;
            }
            let Some(index) = core
                .candidates
                .iter()
                .position(|candidate| candidate.fingerprint == message.fingerprint)
            else {
                continue;
            };
            match message.result {
                Ok((latency, preview)) => {
                    let average = record_latency_sample(core, &message.fingerprint, latency);
                    if let Some(candidate) = core.candidates.get_mut(index) {
                        candidate.latency = Some(average);
                        candidate.latency_class = latency_class(average);
                        candidate.preview = Some(preview);
                        candidate.error = None;
                    }
                }
                Err(error) => {
                    if let Some(candidate) = core.candidates.get_mut(index) {
                        candidate.error = Some(error);
                        candidate.latency_class = LatencyClass::Unknown;
                    }
                }
            }
        }
    }

    fn record_latency_sample(
        core: &mut WizardCore,
        fingerprint: &str,
        sample: Duration,
    ) -> Duration {
        let samples = core
            .latency_samples
            .entry(fingerprint.to_string())
            .or_default();
        samples.push_back(sample);
        while samples.len() > LATENCY_SAMPLE_WINDOW {
            let _ = samples.pop_front();
        }
        average_duration(samples)
    }

    fn average_duration(samples: &VecDeque<Duration>) -> Duration {
        if samples.is_empty() {
            return Duration::ZERO;
        }
        let sum = samples
            .iter()
            .fold(0u128, |sum, sample| sum + sample.as_nanos());
        let average = sum / samples.len() as u128;
        Duration::from_nanos(average.min(u64::MAX as u128) as u64)
    }

    fn preferred_selection_index(
        candidates: &[TargetCandidate],
        preferred_fingerprint: Option<&str>,
    ) -> Option<usize> {
        preferred_fingerprint
            .and_then(|fingerprint| {
                candidates
                    .iter()
                    .position(|candidate| candidate.fingerprint == fingerprint)
            })
            .or_else(|| {
                candidates
                    .iter()
                    .position(|candidate| candidate.error.is_none())
            })
            .or_else(|| (!candidates.is_empty()).then_some(0))
    }

    fn spawn_probe_worker(
        fingerprint: String,
        config: RulerConfig,
        generation: u64,
        tx: Sender<ProbeMessage>,
        stop: Arc<AtomicBool>,
    ) -> Result<JoinHandle<()>, String> {
        let name = format!("ruler-target-probe-{fingerprint}");
        thread::Builder::new()
            .name(name)
            .spawn(move || run_probe_worker(fingerprint, config, generation, tx, stop))
            .map_err(|error| format!("failed to start probe worker: {error}"))
    }

    fn stop_probe_worker_list(workers: &mut Vec<ProbeWorker>) {
        for worker in workers.iter() {
            worker.stop.store(true, Ordering::Relaxed);
        }
        for mut worker in workers.drain(..) {
            if let Some(handle) = worker.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn run_probe_worker(
        fingerprint: String,
        config: RulerConfig,
        generation: u64,
        tx: Sender<ProbeMessage>,
        stop: Arc<AtomicBool>,
    ) {
        while !stop.load(Ordering::Relaxed) {
            let capture_config = match config.to_capture_config() {
                Ok(config) => config,
                Err(error) => {
                    send_probe_error(&tx, generation, &fingerprint, error.to_string());
                    return;
                }
            };
            let mut backend = match create_backend(capture_config) {
                Ok(backend) => backend,
                Err(error) => {
                    send_probe_error(&tx, generation, &fingerprint, error);
                    thread::sleep(Duration::from_millis(PROBE_RECONNECT_PAUSE_MS));
                    continue;
                }
            };
            if let Err(error) = backend.connect() {
                send_probe_error(&tx, generation, &fingerprint, error);
                backend.disconnect();
                thread::sleep(Duration::from_millis(PROBE_RECONNECT_PAUSE_MS));
                continue;
            }

            while !stop.load(Ordering::Relaxed) {
                let start = std::time::Instant::now();
                match backend.capture_frame() {
                    Ok(frame) => {
                        let latency = start.elapsed();
                        let preview = preview_from_captured_frame(frame);
                        if tx
                            .send(ProbeMessage {
                                generation,
                                fingerprint: fingerprint.clone(),
                                result: Ok((latency, preview)),
                            })
                            .is_err()
                        {
                            backend.disconnect();
                            return;
                        }
                    }
                    Err(error) => {
                        send_probe_error(&tx, generation, &fingerprint, error);
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(PROBE_LOOP_PAUSE_MS));
            }
            backend.disconnect();
        }
    }

    fn send_probe_error(
        tx: &Sender<ProbeMessage>,
        generation: u64,
        fingerprint: &str,
        error: String,
    ) {
        let _ = tx.send(ProbeMessage {
            generation,
            fingerprint: fingerprint.to_string(),
            result: Err(error),
        });
    }

    fn preview_from_captured_frame(frame: CapturedFrame) -> PreviewFrame {
        PreviewFrame {
            data: frame.data,
            width: frame.width,
            height: frame.height,
            format: frame.format,
        }
    }

    // ===================== preview pixel conversion =====================

    /// Physical-pixel cap for the downscaled preview, derived from the window
    /// scale and the fixed logical size of the preview pane in `wizard.slint`
    /// (≈224×160). Computed once so the conversion knows its target size.
    fn preview_cap_for_scale(scale: f32) -> (u32, u32) {
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
                            PixelFormat::Bgr => {
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

    // ===================== window plumbing =====================

    fn wizard_scale() -> f32 {
        unsafe { ((GetSystemMetrics(SM_CYSCREEN) as f32) / 720.0).clamp(1.25, 2.5) }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_NCCREATE => {
                let create_struct = lparam.0 as *const CREATESTRUCTW;
                let state_ptr = (*create_struct).lpCreateParams as *mut WizardWindow;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
                LRESULT(1)
            }
            WM_TIMER => {
                tick(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                handle_mouse_move(hwnd, lparam);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                handle_mouse_wheel(hwnd, wparam, lparam);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                handle_left_down(hwnd, lparam);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                handle_left_up(hwnd);
                LRESULT(0)
            }
            WM_CHAR => {
                handle_char(hwnd, wparam);
                LRESULT(0)
            }
            WM_KEYDOWN => {
                handle_key_down(hwnd, wparam);
                LRESULT(0)
            }
            WM_DESTROY => {
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WizardWindow;
                if !state_ptr.is_null() {
                    let _ = Box::from_raw(state_ptr);
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn handle_left_down(hwnd: HWND, lparam: LPARAM) {
        let Some(state) = wizard_window_mut(hwnd) else {
            return;
        };
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);
        state.mouse_down = true;
        state.moved = false;
        state.drag_on_title.set(false);
        state.drag_origin = cursor;
        state.window_origin = POINT {
            x: rect.left,
            y: rect.top,
        };
        let _ = SetCapture(hwnd);
        let position = logical_pos(lparam, state.scale);
        let _ = state
            .window
            .window()
            .try_dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
    }

    unsafe fn handle_mouse_move(hwnd: HWND, lparam: LPARAM) {
        let Some(state) = wizard_window_mut(hwnd) else {
            return;
        };
        if state.mouse_down && state.drag_on_title.get() {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            let dx = cursor.x - state.drag_origin.x;
            let dy = cursor.y - state.drag_origin.y;
            if dx.abs() > 2 || dy.abs() > 2 {
                state.moved = true;
            }
            if state.moved {
                let _ = SetWindowPos(
                    hwnd,
                    HWND::default(),
                    state.window_origin.x + dx,
                    state.window_origin.y + dy,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER,
                );
                return;
            }
        }
        let position = logical_pos(lparam, state.scale);
        let _ = state
            .window
            .window()
            .try_dispatch_event(WindowEvent::PointerMoved { position });
    }

    unsafe fn handle_mouse_wheel(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
        let Some(state) = wizard_window_mut(hwnd) else {
            return;
        };
        // WM_MOUSEWHEEL packs a signed notch delta in the high word of wParam,
        // and — unlike WM_MOUSEMOVE — carries *screen* coordinates in lParam, so
        // round-trip through ScreenToClient before applying the window scale.
        let notches = (((wparam.0 >> 16) & 0xffff) as u16 as i16 as f32) / WHEEL_DELTA_UNIT;
        let mut point = POINT {
            x: (lparam.0 as u32 & 0xffff) as i16 as i32,
            y: ((lparam.0 as u32 >> 16) & 0xffff) as i16 as i32,
        };
        let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut point);
        let position = slint::LogicalPosition::new(
            point.x as f32 / state.scale,
            point.y as f32 / state.scale,
        );
        // Positive delta_y moves the Flickable viewport toward the top, matching
        // a forward (away-from-user) wheel roll — the usual list convention.
        let _ = state
            .window
            .window()
            .try_dispatch_event(WindowEvent::PointerScrolled {
                position,
                delta_x: 0.0,
                delta_y: notches * WHEEL_STEP_LOGICAL_PX,
            });
    }

    unsafe fn handle_left_up(hwnd: HWND) {
        let Some(state) = wizard_window_mut(hwnd) else {
            return;
        };
        let moved = state.moved && state.drag_on_title.get();
        state.mouse_down = false;
        state.moved = false;
        state.drag_on_title.set(false);
        let _ = ReleaseCapture();

        if moved {
            // Finished a title-bar drag; cancel Slint's press grab so it is not
            // read as a click.
            let _ = state
                .window
                .window()
                .try_dispatch_event(WindowEvent::PointerExited);
            return;
        }
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let mut client = cursor;
        let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut client);
        let position = slint::LogicalPosition::new(
            client.x as f32 / state.scale,
            client.y as f32 / state.scale,
        );
        let _ = state
            .window
            .window()
            .try_dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
    }

    fn dispatch_key(window: &Rc<MinimalSoftwareWindow>, text: SharedString) {
        let _ = window
            .window()
            .try_dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        let _ = window
            .window()
            .try_dispatch_event(WindowEvent::KeyReleased { text });
    }

    /// Decode one WM_CHAR UTF-16 code unit into text, buffering the high half of a
    /// surrogate pair across calls. Returns `None` for control characters and for
    /// the (stashed) high surrogate. Mirrors the menu's inline-rename input path.
    fn decode_wm_char(pending_high: &Cell<u16>, unit: u16) -> Option<SharedString> {
        if (0xd800..0xdc00).contains(&unit) {
            pending_high.set(unit);
            return None;
        }
        let units: Vec<u16> = if (0xdc00..0xe000).contains(&unit) {
            let high = pending_high.replace(0);
            if high == 0 {
                return None;
            }
            vec![high, unit]
        } else {
            pending_high.set(0);
            if unit < 0x20 || unit == 0x7f {
                return None;
            }
            vec![unit]
        };
        Some(String::from_utf16_lossy(&units).into())
    }

    /// Forward a typed character to the focused Slint `TextInput` (the manual-panel
    /// parameter fields). Control codes arrive via WM_KEYDOWN instead.
    unsafe fn handle_char(hwnd: HWND, wparam: WPARAM) {
        let Some(state) = wizard_window(hwnd) else {
            return;
        };
        if let Some(text) = decode_wm_char(&state.pending_high, wparam.0 as u16) {
            dispatch_key(&state.window, text);
        }
    }

    /// Esc closes the wizard; editing / navigation keys are forwarded to the
    /// focused Slint `TextInput` (printable characters come through WM_CHAR).
    unsafe fn handle_key_down(hwnd: HWND, wparam: WPARAM) {
        let Some(state) = wizard_window(hwnd) else {
            return;
        };
        let vk = wparam.0 as u16;
        if vk == VK_ESCAPE.0 {
            state.closing.set(true);
            return;
        }
        let key = match vk {
            v if v == VK_BACK.0 => Some(Key::Backspace),
            v if v == VK_DELETE.0 => Some(Key::Delete),
            v if v == VK_LEFT.0 => Some(Key::LeftArrow),
            v if v == VK_RIGHT.0 => Some(Key::RightArrow),
            v if v == VK_HOME.0 => Some(Key::Home),
            v if v == VK_END.0 => Some(Key::End),
            v if v == VK_RETURN.0 => Some(Key::Return),
            _ => None,
        };
        if let Some(key) = key {
            dispatch_key(&state.window, key.into());
        }
    }

    /// What a browse button should pick.
    enum PickKind<'a> {
        /// A folder — the emulator install directory.
        Folder,
        /// A single existing file, restricted to one filter (name, spec).
        File { filter_name: &'a str, filter_spec: &'a str },
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

    unsafe fn wizard_window<'a>(hwnd: HWND) -> Option<&'a WizardWindow> {
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WizardWindow;
        (!state_ptr.is_null()).then(|| &*state_ptr)
    }

    unsafe fn wizard_window_mut<'a>(hwnd: HWND) -> Option<&'a mut WizardWindow> {
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WizardWindow;
        (!state_ptr.is_null()).then(|| &mut *state_ptr)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn preferred_selection_keeps_existing_fingerprint_after_refresh() {
            let candidates = vec![
                candidate("adb:first", None),
                candidate("mumu:selected", None),
                candidate("ld:last", None),
            ];
            assert_eq!(
                preferred_selection_index(&candidates, Some("mumu:selected")),
                Some(1)
            );
        }

        #[test]
        fn preferred_selection_falls_back_to_first_usable_candidate() {
            let candidates = vec![
                candidate("bad", Some("offline")),
                candidate("good", None),
                candidate("later", None),
            ];
            assert_eq!(
                preferred_selection_index(&candidates, Some("missing")),
                Some(1)
            );
        }

        #[test]
        fn average_duration_uses_all_window_samples() {
            let samples = VecDeque::from([
                Duration::from_millis(10),
                Duration::from_millis(20),
                Duration::from_millis(30),
            ]);
            assert_eq!(average_duration(&samples), Duration::from_millis(20));
        }

        #[test]
        fn stop_probe_worker_list_sets_stop_flag_and_joins() {
            let stop = Arc::new(AtomicBool::new(false));
            let observed_stop = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stop);
            let worker_observed_stop = Arc::clone(&observed_stop);
            let handle = thread::spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(1));
                }
                worker_observed_stop.store(true, Ordering::Relaxed);
            });
            let mut workers = vec![ProbeWorker {
                stop,
                handle: Some(handle),
            }];
            stop_probe_worker_list(&mut workers);
            assert!(workers.is_empty());
            assert!(observed_stop.load(Ordering::Relaxed));
        }

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
            assert_eq!(wizard_logical_height(false, false), 404.0);
            assert_eq!(wizard_logical_height(false, true), 404.0);
            // Expanded: recording-only vs the taller manual-target panel.
            assert_eq!(
                wizard_logical_height(true, false),
                404.0 + WIZARD_DEBUG_RECORDING_H
            );
            assert_eq!(
                wizard_logical_height(true, true),
                404.0 + WIZARD_DEBUG_MANUAL_H
            );
            assert!(
                wizard_logical_height(true, true) > wizard_logical_height(true, false),
                "the manual panel is taller than the recording-only panel"
            );
        }

        fn manual_draft(kind: i32) -> ManualDraft {
            ManualDraft {
                kind,
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
            assert_eq!(config.replay_hevc_path.as_deref(), Some("C:/clip.mkv"));
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

        fn candidate(fingerprint: &str, error: Option<&str>) -> TargetCandidate {
            TargetCandidate {
                kind: crate::target_discovery::TargetKind::Adb,
                fingerprint: fingerprint.to_string(),
                name: fingerprint.to_string(),
                detail: String::new(),
                config: RulerConfig {
                    capture_type: "adb".to_string(),
                    install_path: None,
                    instance_index: None,
                    device_id: Some(fingerprint.to_string()),
                    window_handle: None,
                    window_title: None,
                    window_class: None,
                    active_calibration_profile: None,
                    frame_display_mode: Some("0_to_n-1".to_string()),
                    language: Some("zh_CN".to_string()),
                    auto_select_target: false,
                    target_fingerprint: Some(fingerprint.to_string()),
                    overlay_pos_x: None,
                    overlay_pos_y: None,
                    overlay_scale: None,
                    ui_scaler: None,
                    debug_recording_enabled: false,
                    debug_recording_video: false,
                    debug_recording_csv: false,
                    trace_logging_enabled: false,
                    log_output_dir: None,
                    replay_hevc_path: None,
                    replay_fps: None,
                },
                latency: None,
                latency_class: LatencyClass::Unknown,
                preview: None,
                error: error.map(str::to_string),
            }
        }
    }
}
