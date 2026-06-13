use ruler_core::RulerConfig;

use crate::{i18n::I18n, resources::ResourceLocator};

pub fn run_config_wizard(
    _resources: &ResourceLocator,
    i18n: &I18n,
    previous_config: Option<&RulerConfig>,
) -> Option<RulerConfig> {
    platform::run_config_wizard(i18n, previous_config)
}

#[cfg(not(windows))]
mod platform {
    use ruler_core::RulerConfig;

    use crate::i18n::I18n;

    pub fn run_config_wizard(_: &I18n, _: Option<&RulerConfig>) -> Option<RulerConfig> {
        None
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        collections::{HashMap, VecDeque},
        ffi::c_void,
        iter,
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
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{BOOL, COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
            Graphics::Gdi::{
                BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
                InvalidateRect, SetBkMode, SetBrushOrgEx, SetStretchBltMode, SetTextColor,
                StretchDIBits, TextOutW, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
                DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER, HALFTONE, HDC, HGDIOBJ,
                PAINTSTRUCT, SRCCOPY, TRANSPARENT,
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                Controls::{DRAWITEMSTRUCT, ODS_SELECTED},
                WindowsAndMessaging::{
                    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
                    GetSystemMetrics, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW,
                    KillTimer, LoadIconW, MessageBoxW, RegisterClassW, SendMessageW, SetTimer,
                    SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage,
                    BM_GETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, GWLP_USERDATA,
                    HMENU, IDCANCEL, IDI_APPLICATION, LBN_SELCHANGE, LBS_HASSTRINGS, LBS_NOTIFY,
                    LBS_OWNERDRAWFIXED, LB_ADDSTRING, LB_GETCURSEL, LB_RESETCONTENT, LB_SETCURSEL,
                    MB_ICONERROR, MB_OK, MSG, SM_CXSCREEN, SM_CYSCREEN, SWP_NOSIZE, SWP_NOZORDER,
                    SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE,
                    WM_DESTROY, WM_DRAWITEM, WM_NCCREATE, WM_PAINT, WM_TIMER, WNDCLASSW, WS_BORDER,
                    WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
                    WS_VISIBLE, WS_VSCROLL,
                },
            },
        },
    };

    use crate::{
        i18n::I18n,
        target_discovery::{
            discover_targets, latency_class, LatencyClass, PreviewFrame, TargetCandidate,
        },
    };

    const CLASS_NAME: &str = "RulerTargetSelectorWindow";
    const ID_TARGET_LIST: i32 = 2001;
    const ID_REFRESH: i32 = 2002;
    const ID_AUTO_SELECT: i32 = 2003;
    const ID_START: i32 = 2004;
    const ID_STATUS: i32 = 2005;
    const ID_TIMER_PROBE: usize = 1;
    const ID_TIMER_REFRESH: usize = 2;
    const PROBE_UI_INTERVAL_MS: u32 = 200;
    const REFRESH_INTERVAL_MS: u32 = 7000;
    const PROBE_LOOP_PAUSE_MS: u64 = 250;
    const PROBE_RECONNECT_PAUSE_MS: u64 = 1000;
    const LATENCY_SAMPLE_WINDOW: usize = 12;
    const WINDOW_WIDTH: i32 = 840;
    const WINDOW_HEIGHT: i32 = 560;
    const PREVIEW_RECT: RECT = RECT {
        left: 20,
        top: 350,
        right: 430,
        bottom: 505,
    };

    struct WizardState {
        i18n: I18n,
        previous_config: Option<RulerConfig>,
        hwnd: HWND,
        result: Option<RulerConfig>,
        done: bool,
        header_label: HWND,
        target_list: HWND,
        status_label: HWND,
        auto_checkbox: HWND,
        start_button: HWND,
        candidates: Vec<TargetCandidate>,
        selected_index: Option<usize>,
        probe_tx: Sender<ProbeMessage>,
        probe_rx: Receiver<ProbeMessage>,
        probe_generation: u64,
        probe_workers: Vec<ProbeWorker>,
        latency_samples: HashMap<String, VecDeque<Duration>>,
    }

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

    impl WizardState {
        fn new(i18n: &I18n, previous_config: Option<&RulerConfig>) -> Self {
            let (probe_tx, probe_rx) = mpsc::channel();
            Self {
                i18n: i18n.clone(),
                previous_config: previous_config.cloned(),
                hwnd: HWND::default(),
                result: None,
                done: false,
                header_label: HWND::default(),
                target_list: HWND::default(),
                status_label: HWND::default(),
                auto_checkbox: HWND::default(),
                start_button: HWND::default(),
                candidates: Vec::new(),
                selected_index: None,
                probe_tx,
                probe_rx,
                probe_generation: 0,
                probe_workers: Vec::new(),
                latency_samples: HashMap::new(),
            }
        }

        fn selected_candidate(&self) -> Option<&TargetCandidate> {
            self.selected_index
                .and_then(|index| self.candidates.get(index))
        }

        fn selected_fingerprint(&self) -> Option<String> {
            self.selected_candidate()
                .map(|candidate| candidate.fingerprint.clone())
        }
    }

    impl Drop for WizardState {
        fn drop(&mut self) {
            stop_probe_workers(self);
        }
    }

    pub fn run_config_wizard(
        i18n: &I18n,
        previous_config: Option<&RulerConfig>,
    ) -> Option<RulerConfig> {
        unsafe {
            let Ok(module) = GetModuleHandleW(PCWSTR::null()) else {
                return None;
            };
            let class_name = wide(CLASS_NAME);
            let icon = LoadIconW(HINSTANCE::default(), IDI_APPLICATION).unwrap_or_default();
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: HINSTANCE(module.0),
                lpszClassName: PCWSTR(class_name.as_ptr()),
                hIcon: icon,
                ..Default::default()
            };
            let _ = RegisterClassW(&class);

            let mut state = Box::new(WizardState::new(i18n, previous_config));
            let state_ptr = state.as_mut() as *mut WizardState;
            let title = wide(&i18n.tr("config.window.title"));
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0),
                0,
                0,
                WINDOW_WIDTH,
                WINDOW_HEIGHT,
                HWND::default(),
                HMENU::default(),
                HINSTANCE(module.0),
                Some(state_ptr.cast()),
            )
            .ok()?;
            if hwnd.0.is_null() {
                return None;
            }

            center_window(hwnd, WINDOW_WIDTH, WINDOW_HEIGHT);
            let _ = ShowWindow(hwnd, SW_SHOW);

            let mut message = MSG::default();
            while !state.done && GetMessageW(&mut message, HWND::default(), 0, 0).0 > 0 {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }

            state.result.take()
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_NCCREATE => {
                let create_struct =
                    lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
                let state_ptr = (*create_struct).lpCreateParams as *mut WizardState;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
                (*state_ptr).hwnd = hwnd;
                LRESULT(1)
            }
            WM_CREATE => {
                if let Some(state) = state_mut(hwnd) {
                    create_controls(hwnd, state);
                    refresh_candidates(state);
                    let _ = SetTimer(hwnd, ID_TIMER_PROBE, PROBE_UI_INTERVAL_MS, None);
                    let _ = SetTimer(hwnd, ID_TIMER_REFRESH, REFRESH_INTERVAL_MS, None);
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                if let Some(state) = state_mut(hwnd) {
                    let control_id = (wparam.0 & 0xffff) as i32;
                    let notification = ((wparam.0 >> 16) & 0xffff) as u32;
                    match control_id {
                        ID_TARGET_LIST if notification == LBN_SELCHANGE => {
                            select_target_from_list(state);
                        }
                        ID_REFRESH => refresh_candidates(state),
                        ID_START => save_selected_target(state),
                        id if id == IDCANCEL.0 => close_without_result(state),
                        _ => {}
                    }
                }
                LRESULT(0)
            }
            WM_DRAWITEM => {
                if let Some(state) = state_mut(hwnd) {
                    draw_target_list_item(state, lparam);
                }
                LRESULT(1)
            }
            WM_TIMER => {
                if let Some(state) = state_mut(hwnd) {
                    if wparam.0 == ID_TIMER_PROBE {
                        drain_probe_messages(state);
                    } else if wparam.0 == ID_TIMER_REFRESH {
                        refresh_candidates(state);
                    }
                }
                LRESULT(0)
            }
            WM_PAINT => {
                if let Some(state) = state_mut(hwnd) {
                    paint_preview(hwnd, state);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                if let Some(state) = state_mut(hwnd) {
                    close_without_result(state);
                } else {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = KillTimer(hwnd, ID_TIMER_PROBE);
                let _ = KillTimer(hwnd, ID_TIMER_REFRESH);
                if let Some(state) = state_mut(hwnd) {
                    stop_probe_workers(state);
                    state.done = true;
                }
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn create_controls(hwnd: HWND, state: &mut WizardState) {
        state.header_label = create_static(
            hwnd,
            20,
            18,
            700,
            24,
            &state.i18n.tr("config.selector.header"),
        );
        state.target_list = create_control(
            hwnd,
            "LISTBOX",
            "",
            ID_TARGET_LIST,
            20,
            50,
            790,
            280,
            WINDOW_STYLE(
                WS_CHILD.0
                    | WS_VISIBLE.0
                    | WS_TABSTOP.0
                    | WS_BORDER.0
                    | WS_VSCROLL.0
                    | LBS_NOTIFY as u32
                    | LBS_OWNERDRAWFIXED as u32
                    | LBS_HASSTRINGS as u32,
            ),
            WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0),
        );
        create_static(
            hwnd,
            PREVIEW_RECT.left,
            PREVIEW_RECT.top - 24,
            180,
            22,
            &state.i18n.tr("config.selector.preview"),
        );
        state.status_label = create_static_id(
            hwnd,
            ID_STATUS,
            450,
            352,
            360,
            78,
            &state.i18n.tr("config.selector.scanning"),
        );
        state.auto_checkbox = create_control(
            hwnd,
            "BUTTON",
            &state.i18n.tr("config.selector.auto_next"),
            ID_AUTO_SELECT,
            450,
            435,
            360,
            26,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_AUTOCHECKBOX as u32),
            WINDOW_EX_STYLE::default(),
        );
        create_control(
            hwnd,
            "BUTTON",
            &state.i18n.tr("config.selector.refresh"),
            ID_REFRESH,
            520,
            475,
            100,
            34,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        );
        state.start_button = create_control(
            hwnd,
            "BUTTON",
            &state.i18n.tr("config.btn.save_start"),
            ID_START,
            640,
            475,
            120,
            34,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        );
        create_control(
            hwnd,
            "BUTTON",
            &state.i18n.tr("config.btn.cancel"),
            IDCANCEL.0,
            770,
            475,
            60,
            34,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        );
    }

    unsafe fn refresh_candidates(state: &mut WizardState) {
        let preferred_fingerprint = state.selected_fingerprint().or_else(|| {
            state
                .previous_config
                .as_ref()
                .and_then(|config| config.target_fingerprint.clone())
        });
        let previous_probe_state = state
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

        stop_probe_workers(state);
        state.probe_generation = state.probe_generation.wrapping_add(1);
        set_text(
            state.status_label,
            &state.i18n.tr("config.selector.scanning"),
        );
        let _ = SendMessageW(state.target_list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));

        state.candidates = discover_targets(state.previous_config.as_ref());
        for candidate in &mut state.candidates {
            if let Some((latency, latency_class, preview, error)) =
                previous_probe_state.get(&candidate.fingerprint)
            {
                candidate.latency = *latency;
                candidate.latency_class = *latency_class;
                candidate.preview = preview.clone();
                candidate.error = error.clone();
            }
        }
        let active_fingerprints = state
            .candidates
            .iter()
            .map(|candidate| candidate.fingerprint.clone())
            .collect::<std::collections::HashSet<_>>();
        state
            .latency_samples
            .retain(|fingerprint, _| active_fingerprints.contains(fingerprint));

        for candidate in &state.candidates {
            let label = wide(&candidate.list_label());
            let _ = SendMessageW(
                state.target_list,
                LB_ADDSTRING,
                WPARAM(0),
                LPARAM(label.as_ptr() as isize),
            );
        }

        if state.candidates.is_empty() {
            state.selected_index = None;
            set_text(
                state.status_label,
                &state.i18n.tr("config.selector.no_targets"),
            );
            update_header_status(state);
            let _ = InvalidateRect(state.hwnd, Some(&PREVIEW_RECT), BOOL(1));
            return;
        }

        let selected =
            preferred_selection_index(&state.candidates, preferred_fingerprint.as_deref())
                .unwrap_or(0);
        let _ = SendMessageW(state.target_list, LB_SETCURSEL, WPARAM(selected), LPARAM(0));
        state.selected_index = Some(selected);
        start_probe_workers(state);
        update_selected_status(state);
    }

    unsafe fn select_target_from_list(state: &mut WizardState) {
        drain_probe_messages(state);
        let selected = SendMessageW(state.target_list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        state.selected_index = (selected >= 0).then_some(selected as usize);
        update_selected_status(state);
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

    unsafe fn drain_probe_messages(state: &mut WizardState) {
        let mut list_updated = false;
        let mut selected_updated = false;
        while let Ok(message) = state.probe_rx.try_recv() {
            if message.generation != state.probe_generation {
                continue;
            }
            let Some(index) = state
                .candidates
                .iter()
                .position(|candidate| candidate.fingerprint == message.fingerprint)
            else {
                continue;
            };

            match message.result {
                Ok((latency, preview)) => {
                    let average = record_latency_sample(state, &message.fingerprint, latency);
                    if let Some(candidate) = state.candidates.get_mut(index) {
                        candidate.latency = Some(average);
                        candidate.latency_class = latency_class(average);
                        candidate.preview = Some(preview);
                        candidate.error = None;
                    }
                }
                Err(error) => {
                    if let Some(candidate) = state.candidates.get_mut(index) {
                        candidate.error = Some(error);
                        candidate.latency_class = LatencyClass::Unknown;
                    }
                }
            }
            list_updated = true;
            selected_updated |= state.selected_index == Some(index);
        }

        if list_updated {
            let _ = InvalidateRect(state.target_list, None, BOOL(1));
        }
        if selected_updated {
            update_selected_status(state);
        } else if list_updated {
            update_header_status(state);
        }
    }

    fn record_latency_sample(
        state: &mut WizardState,
        fingerprint: &str,
        sample: Duration,
    ) -> Duration {
        let samples = state
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

    fn start_probe_workers(state: &mut WizardState) {
        let probes = state
            .candidates
            .iter()
            .map(|candidate| (candidate.fingerprint.clone(), candidate.config.clone()))
            .collect::<Vec<_>>();

        for (fingerprint, config) in probes {
            let stop = Arc::new(AtomicBool::new(false));
            match spawn_probe_worker(
                fingerprint.clone(),
                config,
                state.probe_generation,
                state.probe_tx.clone(),
                Arc::clone(&stop),
            ) {
                Ok(handle) => {
                    state.probe_workers.push(ProbeWorker {
                        stop,
                        handle: Some(handle),
                    });
                }
                Err(error) => {
                    send_probe_error(&state.probe_tx, state.probe_generation, &fingerprint, error);
                }
            }
        }
    }

    fn stop_probe_workers(state: &mut WizardState) {
        stop_probe_worker_list(&mut state.probe_workers);
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
            .spawn(move || {
                run_probe_worker(fingerprint, config, generation, tx, stop);
            })
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

    unsafe fn update_selected_status(state: &mut WizardState) {
        update_header_status(state);
        if let Some(candidate) = state.selected_candidate() {
            let status = if let Some(error) = &candidate.error {
                state.i18n.tr_with(
                    "config.selector.selected_error",
                    &[("error", error.clone())],
                )
            } else {
                state.i18n.tr_with(
                    "config.selector.selected_ok",
                    &[
                        ("name", candidate.name.clone()),
                        ("latency", candidate.latency_text()),
                    ],
                )
            };
            set_text(state.status_label, &status);
        } else {
            set_text(
                state.status_label,
                &state.i18n.tr("config.selector.no_selection"),
            );
        }
        let _ = InvalidateRect(state.hwnd, Some(&PREVIEW_RECT), BOOL(1));
    }

    unsafe fn update_header_status(state: &mut WizardState) {
        let text = state
            .selected_candidate()
            .and_then(|candidate| candidate.latency.map(|_| candidate.latency_text()))
            .map(|latency| {
                state.i18n.tr_with(
                    "config.selector.header_with_latency",
                    &[("latency", latency)],
                )
            })
            .unwrap_or_else(|| state.i18n.tr("config.selector.header"));
        set_text(state.header_label, &text);
    }

    unsafe fn draw_target_list_item(state: &WizardState, lparam: LPARAM) {
        let draw = &*(lparam.0 as *const DRAWITEMSTRUCT);
        if draw.itemID == u32::MAX {
            return;
        }
        let Some(candidate) = state.candidates.get(draw.itemID as usize) else {
            return;
        };

        let selected = (draw.itemState.0 & ODS_SELECTED.0) != 0;
        let background = if selected {
            COLORREF(0x00E8F2FF)
        } else {
            COLORREF(0x00FFFFFF)
        };
        let brush = CreateSolidBrush(background);
        if !brush.0.is_null() {
            let _ = FillRect(draw.hDC, &draw.rcItem, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
        }

        let mut text_rect = draw.rcItem;
        text_rect.left += 8;
        text_rect.right -= 8;
        let _ = SetBkMode(draw.hDC, TRANSPARENT);
        let _ = SetTextColor(draw.hDC, latency_text_color(candidate));
        let mut text = candidate.list_label().encode_utf16().collect::<Vec<_>>();
        let _ = DrawTextW(
            draw.hDC,
            &mut text,
            &mut text_rect,
            DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
        );
    }

    fn latency_text_color(candidate: &TargetCandidate) -> COLORREF {
        if candidate.error.is_some() {
            return COLORREF(0x003030D8);
        }
        match candidate.latency_class {
            LatencyClass::PaleGreen => COLORREF(0x0060A060),
            LatencyClass::Green => COLORREF(0x00208020),
            LatencyClass::Yellow => COLORREF(0x0000A0B8),
            LatencyClass::Orange => COLORREF(0x000070D8),
            LatencyClass::Red => COLORREF(0x002020D8),
            LatencyClass::Unknown => COLORREF(0x00505050),
        }
    }

    unsafe fn save_selected_target(state: &mut WizardState) {
        let Some(candidate) = state.selected_candidate() else {
            show_error(
                state.hwnd,
                &state.i18n.tr("config.selector.error.no_target.title"),
                &state.i18n.tr("config.selector.error.no_target"),
            );
            return;
        };
        if let Some(error) = &candidate.error {
            show_error(
                state.hwnd,
                &state.i18n.tr("config.selector.error.unavailable.title"),
                &state.i18n.tr_with(
                    "config.selector.error.unavailable",
                    &[("error", error.clone())],
                ),
            );
            return;
        }
        if candidate.preview.is_none() {
            show_error(
                state.hwnd,
                &state.i18n.tr("config.selector.error.unavailable.title"),
                &state.i18n.tr_with(
                    "config.selector.error.unavailable",
                    &[(
                        "error",
                        state.i18n.tr("config.selector.error.waiting_probe"),
                    )],
                ),
            );
            return;
        }

        let mut config = candidate.config.clone();
        config.language = state
            .previous_config
            .as_ref()
            .and_then(|previous| previous.language.clone())
            .or_else(|| Some(state.i18n.locale().to_string()));
        config.auto_select_target =
            SendMessageW(state.auto_checkbox, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == 1;
        config.target_fingerprint = Some(candidate.fingerprint.clone());

        state.result = Some(config);
        stop_probe_workers(state);
        state.done = true;
        let _ = DestroyWindow(state.hwnd);
    }

    unsafe fn paint_preview(hwnd: HWND, state: &WizardState) {
        let mut paint = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut paint);
        if !hdc.0.is_null() {
            draw_preview(
                hdc,
                state
                    .selected_candidate()
                    .and_then(|candidate| candidate.preview.as_ref()),
                &state.i18n,
            );
        }
        let _ = EndPaint(hwnd, &paint);
    }

    unsafe fn draw_preview(hdc: HDC, frame: Option<&PreviewFrame>, i18n: &I18n) {
        let brush = CreateSolidBrush(COLORREF(0x00FFFFFF));
        if !brush.0.is_null() {
            let _ = FillRect(hdc, &PREVIEW_RECT, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
        }

        let Some(frame) = frame else {
            draw_preview_placeholder(hdc, &i18n.tr("config.window.preview.unavailable"));
            return;
        };
        if frame.width == 0 || frame.height == 0 {
            draw_preview_placeholder(hdc, &i18n.tr("config.window.preview.unavailable"));
            return;
        }
        let Ok(buffer) = preview_bgr24_top_down(frame) else {
            draw_preview_placeholder(hdc, &i18n.tr("config.window.preview.error"));
            return;
        };

        let mut bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: frame.width as i32,
                biHeight: -(frame.height as i32),
                biPlanes: 1,
                biBitCount: 24,
                biCompression: BI_RGB.0,
                biSizeImage: buffer.len() as u32,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [Default::default(); 1],
        };

        let target_width = PREVIEW_RECT.right - PREVIEW_RECT.left;
        let target_height = PREVIEW_RECT.bottom - PREVIEW_RECT.top;
        let source_ratio = frame.width as f64 / frame.height as f64;
        let target_ratio = target_width as f64 / target_height as f64;
        let (draw_width, draw_height) = if source_ratio >= target_ratio {
            (
                target_width,
                (target_width as f64 / source_ratio).round() as i32,
            )
        } else {
            (
                (target_height as f64 * source_ratio).round() as i32,
                target_height,
            )
        };
        let x = PREVIEW_RECT.left + (target_width - draw_width) / 2;
        let y = PREVIEW_RECT.top + (target_height - draw_height) / 2;

        // The preview always downscales the captured frame. A fresh paint DC defaults to
        // BLACKONWHITE (STRETCH_ANDSCANS), which bitwise-ANDs discarded pixels into the
        // survivors, collapsing light areas to black with banded artifacts. HALFTONE
        // downsamples cleanly; it requires a SetBrushOrgEx call afterward.
        let _ = SetStretchBltMode(hdc, HALFTONE);
        let _ = SetBrushOrgEx(hdc, 0, 0, None);

        let _ = StretchDIBits(
            hdc,
            x,
            y,
            draw_width,
            draw_height,
            0,
            0,
            frame.width as i32,
            frame.height as i32,
            Some(buffer.as_ptr() as *const c_void),
            &mut bitmap_info,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }

    unsafe fn draw_preview_placeholder(hdc: HDC, text: &str) {
        let text = wide(text);
        let _ = TextOutW(
            hdc,
            PREVIEW_RECT.left + 12,
            PREVIEW_RECT.top + 58,
            &text[..text.len() - 1],
        );
    }

    fn preview_bgr24_top_down(frame: &PreviewFrame) -> Result<Vec<u8>, String> {
        let source_stride = frame
            .width
            .checked_mul(match frame.format {
                PixelFormat::Rgba => 4,
                PixelFormat::Bgr => 3,
            })
            .ok_or_else(|| "preview source stride overflow".to_string())?
            as usize;
        let packed_stride = frame
            .width
            .checked_mul(3)
            .ok_or_else(|| "preview stride overflow".to_string())?
            as usize;
        let stride = (packed_stride + 3) & !3;
        let mut output = vec![0u8; stride * frame.height as usize];
        for y in 0..frame.height as usize {
            let src_y = frame.height as usize - 1 - y;
            for x in 0..frame.width as usize {
                let dst = y * stride + x * 3;
                match frame.format {
                    PixelFormat::Rgba => {
                        let src = src_y * source_stride + x * 4;
                        if src + 3 >= frame.data.len() {
                            return Err("preview RGBA buffer is too short".to_string());
                        }
                        output[dst] = frame.data[src + 2];
                        output[dst + 1] = frame.data[src + 1];
                        output[dst + 2] = frame.data[src];
                    }
                    PixelFormat::Bgr => {
                        let src = src_y * source_stride + x * 3;
                        if src + 2 >= frame.data.len() {
                            return Err("preview BGR buffer is too short".to_string());
                        }
                        output[dst] = frame.data[src];
                        output[dst + 1] = frame.data[src + 1];
                        output[dst + 2] = frame.data[src + 2];
                    }
                }
            }
        }
        Ok(output)
    }

    unsafe fn close_without_result(state: &mut WizardState) {
        stop_probe_workers(state);
        state.result = None;
        state.done = true;
        let _ = DestroyWindow(state.hwnd);
    }

    unsafe fn create_static(
        hwnd: HWND,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        text: &str,
    ) -> HWND {
        create_static_id(hwnd, 0, x, y, width, height, text)
    }

    unsafe fn create_static_id(
        hwnd: HWND,
        id: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        text: &str,
    ) -> HWND {
        create_control(
            hwnd,
            "STATIC",
            text,
            id,
            x,
            y,
            width,
            height,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
            WINDOW_EX_STYLE::default(),
        )
    }

    unsafe fn create_control(
        parent: HWND,
        class_name: &str,
        text: &str,
        id: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        style: WINDOW_STYLE,
        ex_style: WINDOW_EX_STYLE,
    ) -> HWND {
        let Ok(module) = GetModuleHandleW(PCWSTR::null()) else {
            return HWND::default();
        };
        let class_name = wide(class_name);
        let text = wide(text);
        CreateWindowExW(
            ex_style,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(text.as_ptr()),
            style,
            x,
            y,
            width,
            height,
            parent,
            child_id(id),
            HINSTANCE(module.0),
            None,
        )
        .unwrap_or_default()
    }

    unsafe fn center_window(hwnd: HWND, width: i32, height: i32) {
        let screen_width = GetSystemMetrics(SM_CXSCREEN);
        let screen_height = GetSystemMetrics(SM_CYSCREEN);
        let x = (screen_width - width).max(0) / 2;
        let y = (screen_height - height).max(0) / 2;
        let _ = SetWindowPos(hwnd, HWND::default(), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
    }

    unsafe fn set_text(hwnd: HWND, text: &str) {
        let text = wide(text);
        let _ = SetWindowTextW(hwnd, PCWSTR(text.as_ptr()));
    }

    unsafe fn show_error(hwnd: HWND, title: &str, message: &str) {
        let title = wide(title);
        let message = wide(message);
        let _ = MessageBoxW(
            hwnd,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }

    unsafe fn state_mut<'a>(hwnd: HWND) -> Option<&'a mut WizardState> {
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WizardState;
        if state_ptr.is_null() {
            None
        } else {
            Some(&mut *state_ptr)
        }
    }

    fn child_id(id: i32) -> HMENU {
        if id == 0 {
            HMENU::default()
        } else {
            HMENU(id as isize as *mut c_void)
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(iter::once(0)).collect()
    }

    #[allow(dead_code)]
    unsafe fn edit_text(hwnd: HWND) -> String {
        let len = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; len.max(0) as usize + 1];
        let count = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..count as usize])
    }

    #[allow(dead_code)]
    fn latency_color_class(class: LatencyClass) -> &'static str {
        match class {
            LatencyClass::PaleGreen => "pale-green",
            LatencyClass::Green => "green",
            LatencyClass::Yellow => "yellow",
            LatencyClass::Orange => "orange",
            LatencyClass::Red => "red",
            LatencyClass::Unknown => "unknown",
        }
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
        fn preview_rgba_bottom_up_to_bgr24_top_down_keeps_channels() {
            let frame = PreviewFrame {
                width: 2,
                height: 2,
                format: PixelFormat::Rgba,
                data: vec![
                    255, 0, 0, 255, 0, 255, 0, 255, //
                    0, 0, 255, 255, 255, 255, 255, 255,
                ],
            };

            assert_eq!(
                preview_bgr24_top_down(&frame).unwrap(),
                vec![
                    255, 0, 0, 255, 255, 255, 0, 0, //
                    0, 0, 255, 0, 255, 0, 0, 0,
                ]
            );
        }

        #[test]
        fn preview_bgr_bottom_up_to_bgr24_top_down_keeps_channels() {
            let frame = PreviewFrame {
                width: 2,
                height: 2,
                format: PixelFormat::Bgr,
                data: vec![
                    0, 0, 255, 0, 255, 0, //
                    255, 0, 0, 255, 255, 255,
                ],
            };

            assert_eq!(
                preview_bgr24_top_down(&frame).unwrap(),
                vec![
                    255, 0, 0, 255, 255, 255, 0, 0, //
                    0, 0, 255, 0, 255, 0, 0, 0,
                ]
            );
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
                },
                latency: None,
                latency_class: LatencyClass::Unknown,
                preview: None,
                error: error.map(str::to_string),
            }
        }
    }
}
