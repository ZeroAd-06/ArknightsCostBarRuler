use std::{
    fmt,
    sync::{mpsc::Sender, Arc},
};

use crate::{commands::UiCommand, i18n::I18n, icons::IconSet, worker::SharedAppState};

/// Initial overlay window placement, sourced from the persisted config.
#[derive(Clone, Copy, Debug, Default)]
pub struct OverlayPlacement {
    pub pos: Option<(i32, i32)>,
    pub scale_mult: f32,
}

impl OverlayPlacement {
    #[must_use]
    pub fn scale_or_default(self) -> f32 {
        if self.scale_mult > 0.1 {
            self.scale_mult
        } else {
            1.0
        }
    }
}

pub struct OverlayRuntime {
    state: Arc<SharedAppState>,
    command_tx: Sender<UiCommand>,
    i18n: Arc<I18n>,
    icons: Arc<IconSet>,
    placement: OverlayPlacement,
}

impl fmt::Debug for OverlayRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OverlayRuntime")
            .field("state", &self.state.snapshot())
            .finish_non_exhaustive()
    }
}

impl OverlayRuntime {
    #[must_use]
    pub fn new(
        state: Arc<SharedAppState>,
        command_tx: Sender<UiCommand>,
        i18n: Arc<I18n>,
        icons: Arc<IconSet>,
        placement: OverlayPlacement,
    ) -> Self {
        Self {
            state,
            command_tx,
            i18n,
            icons,
            placement,
        }
    }

    #[must_use]
    pub fn startup_note(&self) -> String {
        let snapshot = self.state.snapshot();
        format!(
            "slint overlay runtime registered with mode={:?}",
            snapshot.ui.mode
        )
    }

    pub fn run(&self) -> Result<(), OverlayError> {
        platform::run(
            Arc::clone(&self.state),
            self.command_tx.clone(),
            Arc::clone(&self.i18n),
            Arc::clone(&self.icons),
            self.placement,
        )
    }
}

#[derive(Debug)]
pub struct OverlayError {
    message: String,
}

impl OverlayError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for OverlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OverlayError {}

#[cfg(not(windows))]
mod platform {
    use std::sync::{mpsc::Sender, Arc};

    use crate::{commands::UiCommand, i18n::I18n, icons::IconSet, worker::SharedAppState};

    use super::OverlayError;

    pub fn run(
        _: Arc<SharedAppState>,
        _: Sender<UiCommand>,
        _: Arc<I18n>,
        _: Arc<IconSet>,
        _: super::OverlayPlacement,
    ) -> Result<(), OverlayError> {
        Err(OverlayError::new(
            "native overlay window is currently implemented for Windows only",
        ))
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        cell::Cell,
        rc::Rc,
        sync::{
            atomic::{AtomicIsize, Ordering},
            mpsc::Sender,
            Arc,
        },
        time::Instant,
    };

    use ruler_core::analysis::roi::find_cost_bar_roi;
    use slint::{
        platform::{
            software_renderer::{MinimalSoftwareWindow, SoftwareRenderer},
            Key, PointerEventButton, WindowAdapter, WindowEvent,
        },
        ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, SharedString, VecModel,
    };

    use super::OverlayError;
    use crate::{
        commands::UiCommand,
        i18n::I18n,
        icons::IconSet,
        menu,
        slint_win::{
            client_xy, create_dib, ensure_platform, logical_pos, present_layered, wide, PreBgra,
            WindowSlot,
        },
        tray,
        ui::{Hud, HudMode, ProfileRow, RulerMenu},
        ui_state::{FrameDisplayMode, OverlayMode},
        worker::SharedAppState,
    };
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
            Graphics::Gdi::{DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ},
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                Controls::WM_MOUSELEAVE,
                Input::KeyboardAndMouse::{
                    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
                    VK_BACK, VK_DELETE, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT,
                },
                Shell::NOTIFYICONDATAW,
                WindowsAndMessaging::{
                    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
                    GetMessageW, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, LoadCursorW,
                    PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer,
                    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, CREATESTRUCTW,
                    CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HICON, HMENU, IDC_ARROW, MSG,
                    SM_CXSCREEN, SM_CYSCREEN, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_SHOW,
                    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CHAR, WM_DESTROY, WM_KEYDOWN,
                    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_NCHITTEST,
                    WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
                    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
                },
            },
        },
    };

    const OVERLAY_TIMER_ID: usize = 1;
    const OVERLAY_TIMER_INTERVAL_MS: u32 = 16;
    const WM_OVERLAY_WAKE: u32 = WM_APP + 2;
    const WM_TRAYICON: u32 = WM_APP + 1;

    // Fixed logical design size of `hud.slint`. Physical size = logical * scale.
    // The panel height stays the scale anchor; extra host height is for controls
    // rendered outside the panel, not extra internal HUD content.
    const LOGICAL_W: f32 = 210.0;
    const LOGICAL_PANEL_H: f32 = 56.0;
    const LOGICAL_H: f32 = 82.0;
    const LOGICAL_TOOLBAR_W: f32 = 108.0;
    const LOGICAL_TOOLBAR_H: f32 = 24.0;
    const LOGICAL_TOOLBAR_RIGHT_PAD: f32 = 4.0;
    const LOGICAL_TOOLBAR_TOP_GAP: f32 = 2.0;
    const HTTRANSPARENT_RESULT: isize = -1;

    struct WindowState {
        hud: Hud,
        window: Rc<MinimalSoftwareWindow>,
        state: Arc<SharedAppState>,
        command_tx: Sender<UiCommand>,
        i18n: Arc<I18n>,
        // software framebuffer backed by a DIB section (no copy on present)
        mem_dc: HDC,
        dib: HBITMAP,
        bits: *mut PreBgra,
        buf_w: usize,
        buf_h: usize,
        scale: f32,
        base_scale: f32,
        scale_mult: f32,
        // factory slot, used to claim windows for menu/dialog popups
        window_slot: WindowSlot,
        // tray icon bound to this window (Shell_NotifyIcon)
        nid: NOTIFYICONDATAW,
        tray_icon: Option<HICON>,
        // guards against a re-entrant popup (e.g. tray click while menu is open)
        menu_open: bool,
        // pointer / drag bookkeeping
        mouse_down: bool,
        moved: bool,
        drag_on_bg: Rc<Cell<bool>>,
        drag_origin: POINT,
        window_origin: POINT,
        tracking_leave: bool,
    }

    impl Drop for WindowState {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteDC(self.mem_dc);
                let _ = DeleteObject(HGDIOBJ(self.dib.0));
            }
        }
    }

    pub fn run(
        shared_state: Arc<SharedAppState>,
        command_tx: Sender<UiCommand>,
        i18n: Arc<I18n>,
        icons: Arc<IconSet>,
        placement: super::OverlayPlacement,
    ) -> Result<(), OverlayError> {
        // Shared, idempotent platform init: the config wizard may have already
        // installed it (set_platform is once-per-process). NewBuffer repaint and
        // bundled fonts are configured there.
        let slot: WindowSlot = ensure_platform();

        let hud = Hud::new().map_err(|error| {
            OverlayError::new(format!("failed to build HUD component: {error}"))
        })?;
        let window = slot
            .borrow_mut()
            .take()
            .ok_or_else(|| OverlayError::new("platform did not produce a HUD window"))?;

        let scale_mult = placement.scale_or_default();
        let (left, top, width, height, base_scale) = initial_geometry(placement.pos, scale_mult);
        let scale = base_scale * scale_mult;

        // Drive layout at the fixed logical design size via the scale factor.
        window
            .window()
            .try_dispatch_event(WindowEvent::ScaleFactorChanged {
                scale_factor: scale,
            })
            .map_err(|error| OverlayError::new(format!("scale dispatch failed: {error}")))?;
        window.set_size(PhysicalSize::new(width as u32, height as u32));

        let drag_on_bg = Rc::new(Cell::new(false));
        let hwnd_cell = Rc::new(Cell::new(0isize));
        wire_callbacks(&hud, &shared_state, &command_tx, &drag_on_bg, &hwnd_cell);

        hud.show()
            .map_err(|error| OverlayError::new(format!("failed to show HUD: {error}")))?;

        unsafe {
            let instance = GetModuleHandleW(PCWSTR::null())
                .map_err(|error| OverlayError::new(format!("GetModuleHandleW failed: {error}")))?;
            let class_name = wide("RulerOverlayWindowClass");
            let title = wide("Arknights Cost Bar Ruler");

            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                hInstance: HINSTANCE(instance.0),
                lpszClassName: PCWSTR(class_name.as_ptr()),
                hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW)
                    .map_err(|error| OverlayError::new(format!("LoadCursorW failed: {error}")))?,
                ..Default::default()
            };
            let atom = RegisterClassW(&class);
            if atom == 0 {
                return Err(OverlayError::new("RegisterClassW failed"));
            }

            let (mem_dc, dib, bits) = create_dib(width, height)
                .ok_or_else(|| OverlayError::new("failed to create framebuffer DIB"))?;

            let window_state = Box::new(WindowState {
                hud,
                window: window.clone(),
                state: Arc::clone(&shared_state),
                command_tx,
                i18n,
                mem_dc,
                dib,
                bits,
                buf_w: width as usize,
                buf_h: height as usize,
                scale,
                base_scale,
                scale_mult,
                window_slot: Rc::clone(&slot),
                nid: NOTIFYICONDATAW::default(),
                tray_icon: None,
                menu_open: false,
                mouse_down: false,
                moved: false,
                drag_on_bg,
                drag_origin: POINT::default(),
                window_origin: POINT::default(),
                tracking_leave: false,
            });
            let state_ptr = Box::into_raw(window_state);

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_LAYERED.0 | WS_EX_TOOLWINDOW.0),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WINDOW_STYLE(WS_POPUP.0 | WS_VISIBLE.0),
                left,
                top,
                width,
                height,
                HWND::default(),
                HMENU::default(),
                HINSTANCE(instance.0),
                Some(state_ptr.cast()),
            )
            .map_err(|error| OverlayError::new(format!("CreateWindowExW failed: {error}")))?;

            if hwnd.0.is_null() {
                let _ = Box::from_raw(state_ptr);
                return Err(OverlayError::new("CreateWindowExW returned null"));
            }
            hwnd_cell.set(hwnd.0 as isize);

            let overlay_hwnd = Arc::new(AtomicIsize::new(hwnd.0 as isize));
            let overlay_waker_hwnd = Arc::clone(&overlay_hwnd);
            shared_state.set_overlay_waker(Some(Arc::new(move || {
                let raw_hwnd = overlay_waker_hwnd.load(Ordering::Relaxed);
                if raw_hwnd != 0 {
                    let _ = PostMessageW(
                        HWND(raw_hwnd as *mut _),
                        WM_OVERLAY_WAKE,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            })));

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetTimer(hwnd, OVERLAY_TIMER_ID, OVERLAY_TIMER_INTERVAL_MS, None);
            let _ = PostMessageW(hwnd, WM_OVERLAY_WAKE, WPARAM(0), LPARAM(0));

            // Tray icon now lives on the UI thread alongside the HUD, so its
            // right-click can open the same Slint menu.
            if let Some((nid, tray_icon)) = tray::win32::install(hwnd, &icons, WM_TRAYICON) {
                if let Some(state) = window_state_mut(hwnd) {
                    state.nid = nid;
                    state.tray_icon = tray_icon;
                }
            }

            let mut message = MSG::default();
            loop {
                let result = GetMessageW(&mut message, HWND::default(), 0, 0).0;
                if result == -1 {
                    return Err(OverlayError::new("GetMessageW failed"));
                }
                if result == 0 {
                    break;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }

            overlay_hwnd.store(0, Ordering::Relaxed);
            shared_state.set_overlay_waker(None);
            Ok(())
        }
    }

    fn wire_callbacks(
        hud: &Hud,
        state: &Arc<SharedAppState>,
        command_tx: &Sender<UiCommand>,
        drag_on_bg: &Rc<Cell<bool>>,
        _hwnd_cell: &Rc<Cell<isize>>,
    ) {
        hud.on_start_clicked({
            let tx = command_tx.clone();
            move || {
                let _ = tx.send(UiCommand::StartCalibration);
            }
        });
        hud.on_reset_clicked({
            let tx = command_tx.clone();
            move || {
                let _ = tx.send(UiCommand::ResetTimer);
            }
        });
        hud.on_undo_reset_clicked({
            let tx = command_tx.clone();
            move || {
                let _ = tx.send(UiCommand::UndoResetTimer);
            }
        });
        hud.on_lap_clicked({
            let tx = command_tx.clone();
            move || {
                let _ = tx.send(UiCommand::ToggleLapTimer);
            }
        });
        hud.on_back_cycle({
            let tx = command_tx.clone();
            let state = Arc::clone(state);
            move || {
                let frames = state.snapshot().ui.total_frames_in_cycle;
                let _ = tx.send(UiCommand::AdjustTimer { frames: -frames });
            }
        });
        hud.on_fwd_cycle({
            let tx = command_tx.clone();
            let state = Arc::clone(state);
            move || {
                let frames = state.snapshot().ui.total_frames_in_cycle;
                let _ = tx.send(UiCommand::AdjustTimer { frames });
            }
        });
        hud.on_bg_pressed({
            let drag_on_bg = Rc::clone(drag_on_bg);
            move || drag_on_bg.set(true)
        });
        hud.on_bg_released({
            let drag_on_bg = Rc::clone(drag_on_bg);
            move || drag_on_bg.set(false)
        });
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
                let state_ptr = (*create_struct).lpCreateParams as *mut WindowState;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
                LRESULT(1)
            }
            WM_PAINT => {
                // Painting is driven by the timer via UpdateLayeredWindow; just validate.
                let mut paint = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
                let hdc = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut paint);
                let _ = hdc;
                let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &paint);
                LRESULT(0)
            }
            WM_NCHITTEST => {
                if let Some(state) = window_state_mut(hwnd) {
                    if outer_area_should_pass_through(hwnd, state) {
                        return LRESULT(HTTRANSPARENT_RESULT);
                    }
                }
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
            WM_TIMER => {
                if should_exit(hwnd) {
                    let _ = DestroyWindow(hwnd);
                } else {
                    tick(hwnd);
                }
                LRESULT(0)
            }
            WM_OVERLAY_WAKE => {
                // Worker notifications can arrive at ~1 kHz; rendering is
                // throttled to the 16 ms timer, so only act on a prompt exit.
                if should_exit(hwnd) {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                handle_left_down(hwnd, lparam);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                handle_mouse_move(hwnd, lparam);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                if let Some(state) = window_state_mut(hwnd) {
                    state.tracking_leave = false;
                    let _ = state
                        .window
                        .window()
                        .try_dispatch_event(WindowEvent::PointerExited);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                handle_left_up(hwnd);
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                show_menu(hwnd);
                LRESULT(0)
            }
            WM_TRAYICON => {
                // Shell forwards the mouse action in the low word of lparam.
                if lparam.0 as u32 == WM_RBUTTONUP {
                    if let Some(state) = window_state_mut(hwnd) {
                        let snapshot = state.state.snapshot();
                        let tip = match snapshot.ui.mode {
                            OverlayMode::Running => "明日方舟费用条尺子".to_string(),
                            _ => snapshot.ui.message.clone(),
                        };
                        tray::win32::update_tooltip(&mut state.nid, &tip);
                    }
                    show_menu(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
                if !state_ptr.is_null() {
                    (*state_ptr).state.set_overlay_waker(None);
                    tray::win32::remove(&(*state_ptr).nid, (*state_ptr).tray_icon);
                    let _ = Box::from_raw(state_ptr);
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn tick(hwnd: HWND) {
        slint::platform::update_timers_and_animations();
        let Some(state) = window_state_mut(hwnd) else {
            return;
        };
        let snapshot = state.state.snapshot();

        let want_mult = f32::from(snapshot.ui.overlay_scale_pct) / 100.0;
        if (want_mult - state.scale_mult).abs() > 0.001 {
            apply_scale(hwnd, state, want_mult);
        }

        sync_properties(state, &snapshot.ui);

        let w = state.buf_w;
        let h = state.buf_h;
        let bits = state.bits;
        let drawn = state.window.draw_if_needed(|renderer: &SoftwareRenderer| {
            let buffer = unsafe { std::slice::from_raw_parts_mut(bits, w * h) };
            renderer.render(buffer, w);
        });
        if drawn {
            present_layered(hwnd, state.mem_dc, w as i32, h as i32);
            state
                .state
                .record_overlay_paint(snapshot.worker_timing.sample_index, Instant::now());
        }
    }

    unsafe fn apply_scale(hwnd: HWND, state: &mut WindowState, new_mult: f32) {
        let effective = state.base_scale * new_mult;
        let width = (LOGICAL_W * effective).round() as i32;
        let height = (LOGICAL_H * effective).round() as i32;

        // Rebuild the framebuffer DIB at the new physical size.
        let _ = DeleteDC(state.mem_dc);
        let _ = DeleteObject(HGDIOBJ(state.dib.0));
        if let Some((mem_dc, dib, bits)) = create_dib(width, height) {
            state.mem_dc = mem_dc;
            state.dib = dib;
            state.bits = bits;
            state.buf_w = width as usize;
            state.buf_h = height as usize;
        }

        state.scale = effective;
        state.scale_mult = new_mult;
        let _ = state
            .window
            .window()
            .try_dispatch_event(WindowEvent::ScaleFactorChanged {
                scale_factor: effective,
            });
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

    fn sync_properties(state: &WindowState, ui: &crate::ui_state::UiSnapshot) {
        let hud = &state.hud;

        hud.set_mode(map_mode(&ui.mode));
        hud.set_time_str(ui.time_str.as_str().into());
        hud.set_frame_str(ui.display_frame.as_str().into());
        hud.set_undo_reset_enabled(ui.can_undo_reset);

        let negative = ui.display_total.ends_with('*');
        let total_clean = ui.display_total.trim_end_matches('*');
        hud.set_total_str(total_clean.into());
        hud.set_cost_negative(negative);

        let lap = ui
            .lap_frames
            .map(|frames| frames.to_string())
            .unwrap_or_default();
        hud.set_lap_str(lap.into());

        hud.set_progress(f32::from(ui.progress_percent));
        hud.set_progress_str(format!("{}%", ui.progress_percent).into());

        let message = match ui.mode {
            OverlayMode::Idle => state.i18n.tr("overlay.msg.idle"),
            OverlayMode::PreCalibration => state.i18n.tr("overlay.msg.pre_cal"),
            OverlayMode::Error | OverlayMode::Booting => ui.message.clone(),
            _ => String::new(),
        };
        hud.set_message(message.into());
    }

    fn map_mode(mode: &OverlayMode) -> HudMode {
        match mode {
            OverlayMode::Booting => HudMode::Booting,
            OverlayMode::Idle => HudMode::Idle,
            OverlayMode::PreCalibration => HudMode::Precal,
            OverlayMode::Calibrating => HudMode::Calibrating,
            OverlayMode::Running => HudMode::Running,
            OverlayMode::Error => HudMode::Error,
        }
    }

    unsafe fn handle_left_down(hwnd: HWND, lparam: LPARAM) {
        let Some(state) = window_state_mut(hwnd) else {
            return;
        };
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let mut window_rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut window_rect);

        state.mouse_down = true;
        state.moved = false;
        state.drag_on_bg.set(false);
        state.drag_origin = cursor;
        state.window_origin = POINT {
            x: window_rect.left,
            y: window_rect.top,
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
        let Some(state) = window_state_mut(hwnd) else {
            return;
        };

        if !state.tracking_leave {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            if TrackMouseEvent(&mut tme).is_ok() {
                state.tracking_leave = true;
            }
        }

        if state.mouse_down {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            let dx = cursor.x - state.drag_origin.x;
            let dy = cursor.y - state.drag_origin.y;
            if dx.abs() > 2 || dy.abs() > 2 {
                state.moved = true;
            }
            if state.moved && state.drag_on_bg.get() {
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

    unsafe fn handle_left_up(hwnd: HWND) {
        let Some(state) = window_state_mut(hwnd) else {
            return;
        };
        let moved = state.moved;
        let dragged_bg = state.drag_on_bg.get();
        state.mouse_down = false;
        state.moved = false;
        state.drag_on_bg.set(false);
        let _ = ReleaseCapture();

        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let mut client = cursor;
        let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut client);
        let position =
            LogicalPosition::new(client.x as f32 / state.scale, client.y as f32 / state.scale);

        if moved && dragged_bg {
            // Drag finished — cancel Slint's press grab so it is not read as a click.
            let _ = state
                .window
                .window()
                .try_dispatch_event(WindowEvent::PointerExited);
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_ok() {
                let _ = state.command_tx.send(UiCommand::SaveOverlayPlacement {
                    x: rect.left,
                    y: rect.top,
                });
            }
        } else {
            let _ = state
                .window
                .window()
                .try_dispatch_event(WindowEvent::PointerReleased {
                    position,
                    button: PointerEventButton::Left,
                });
        }
    }

    // ===================== context menu (Slint popup) =====================

    const MENU_TIMER_ID: usize = 2;
    const MENU_LOGICAL_W: f32 = 300.0;
    const MENU_ROW_H: f32 = 30.0; // mirror of `row-h` in menu.slint
    const MENU_DIV_H: f32 = 12.0; // mirror of divider rows
    const MENU_PAD_V: f32 = 8.0; // mirror of vertical padding

    /// Logical menu height for `n` profiles. Mirrors the fixed vertical metrics of
    /// menu.slint: `2*pad + (header + n*profile + display + scale + timer + footer)
    /// rows + 2 dividers`.
    fn menu_logical_height(n_profiles: usize) -> f32 {
        MENU_PAD_V * 2.0 + (5.0 + n_profiles as f32) * MENU_ROW_H + 2.0 * MENU_DIV_H
    }

    struct MenuState {
        menu: RulerMenu,
        window: Rc<MinimalSoftwareWindow>,
        state: Arc<SharedAppState>,
        mem_dc: HDC,
        dib: HBITMAP,
        bits: *mut PreBgra,
        buf_w: usize,
        buf_h: usize,
        scale: f32,
        closing: Rc<Cell<bool>>,
        // high half of a pending UTF-16 surrogate pair (inline-rename IME input)
        pending_high: Cell<u16>,
        // last-seen profile list, so the model is refreshed / popup resized on change
        profiles_sig: String,
        profile_count: usize,
    }

    impl Drop for MenuState {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteDC(self.mem_dc);
                let _ = DeleteObject(HGDIOBJ(self.dib.0));
            }
        }
    }

    fn display_mode_index(mode: FrameDisplayMode) -> i32 {
        match mode {
            FrameDisplayMode::ZeroToNMinusOne => 0,
            FrameDisplayMode::ZeroToN => 1,
            FrameDisplayMode::OneToN => 2,
        }
    }

    fn index_to_display_mode(index: i32) -> FrameDisplayMode {
        match index {
            1 => FrameDisplayMode::ZeroToN,
            2 => FrameDisplayMode::OneToN,
            _ => FrameDisplayMode::ZeroToNMinusOne,
        }
    }

    fn scale_pct_to_index(pct: u16) -> i32 {
        match pct {
            75 => 0,
            125 => 2,
            150 => 3,
            _ => 1,
        }
    }

    fn index_to_scale(index: i32) -> f32 {
        match index {
            0 => 0.75,
            2 => 1.25,
            3 => 1.5,
            _ => 1.0,
        }
    }

    unsafe fn show_menu(parent: HWND) {
        // Re-entrancy guard: the tray callback can arrive on this window even
        // while a menu's nested loop is running (Shell_NotifyIcon ignores our
        // mouse capture), which would otherwise stack a second popup.
        match window_state_mut(parent) {
            Some(state) if !state.menu_open => state.menu_open = true,
            _ => return,
        }

        let Some(parent_state) = window_state(parent) else {
            return;
        };
        let state = Arc::clone(&parent_state.state);
        let command_tx = parent_state.command_tx.clone();
        let i18n = Arc::clone(&parent_state.i18n);
        let slot = Rc::clone(&parent_state.window_slot);
        let scale = parent_state.scale;

        let snapshot = state.snapshot();
        let n_profiles = snapshot.ui.profiles.len();

        let Ok(menu) = RulerMenu::new() else {
            return;
        };
        let Some(menu_window) = slot.borrow_mut().take() else {
            return;
        };

        populate_menu(&menu, &snapshot.ui, &i18n);

        let closing = Rc::new(Cell::new(false));
        wire_menu_callbacks(&menu, &command_tx, &state, &closing);

        let width = (MENU_LOGICAL_W * scale).round() as i32;
        let height = (menu_logical_height(n_profiles) * scale).round() as i32;
        let _ = menu_window
            .window()
            .try_dispatch_event(WindowEvent::ScaleFactorChanged {
                scale_factor: scale,
            });
        menu_window.set_size(PhysicalSize::new(width as u32, height as u32));
        let _ = menu.show();
        // Activate so an inline-rename TextInput can hold focus and receive keys.
        let _ = menu_window
            .window()
            .try_dispatch_event(WindowEvent::WindowActiveChanged(true));

        // Pop at the cursor, clamped fully on-screen.
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let screen_h = GetSystemMetrics(SM_CYSCREEN);
        let left = cursor.x.min((screen_w - width).max(0)).max(0);
        let top = cursor.y.min((screen_h - height).max(0)).max(0);

        let Some((mem_dc, dib, bits)) = create_dib(width, height) else {
            return;
        };

        let menu_state = Box::new(MenuState {
            menu,
            window: menu_window,
            state: Arc::clone(&state),
            mem_dc,
            dib,
            bits,
            buf_w: width as usize,
            buf_h: height as usize,
            scale,
            closing: Rc::clone(&closing),
            pending_high: Cell::new(0),
            profiles_sig: profiles_signature(&snapshot.ui.profiles),
            profile_count: n_profiles,
        });
        let state_ptr = Box::into_raw(menu_state);

        let Ok(instance) = GetModuleHandleW(PCWSTR::null()) else {
            let _ = Box::from_raw(state_ptr);
            return;
        };
        let class_name = wide("RulerMenuWindowClass");
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(menu_window_proc),
            hInstance: HINSTANCE(instance.0),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        // Re-registration on subsequent opens returns 0; the class persists, so
        // the error is expected and ignored.
        RegisterClassW(&class);

        let title = wide("Ruler Menu");
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_LAYERED.0 | WS_EX_TOOLWINDOW.0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WINDOW_STYLE(WS_POPUP.0 | WS_VISIBLE.0),
            left,
            top,
            width,
            height,
            parent,
            HMENU::default(),
            HINSTANCE(instance.0),
            Some(state_ptr.cast()),
        ) else {
            let _ = Box::from_raw(state_ptr);
            return;
        };
        if hwnd.0.is_null() {
            let _ = Box::from_raw(state_ptr);
            return;
        }

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetCapture(hwnd);
        let _ = SetTimer(hwnd, MENU_TIMER_ID, OVERLAY_TIMER_INTERVAL_MS, None);
        menu_tick(hwnd);

        // Nested loop. The HUD's own WM_TIMER keeps firing on this thread, so the
        // timer/readout behind the menu stays live.
        let mut message = MSG::default();
        while !closing.get() {
            let result = GetMessageW(&mut message, HWND::default(), 0, 0).0;
            if result == 0 {
                // WM_QUIT consumed here; re-post so the outer loop also exits.
                PostQuitMessage(0);
                break;
            }
            if result == -1 {
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        let _ = ReleaseCapture();
        let _ = DestroyWindow(hwnd);

        // Clear the guard (no-op if the window was destroyed while open).
        if let Some(state) = window_state_mut(parent) {
            state.menu_open = false;
        }
    }

    unsafe fn populate_menu(menu: &RulerMenu, ui: &crate::ui_state::UiSnapshot, i18n: &I18n) {
        rebuild_profiles(menu, &ui.profiles);
        menu.set_editing_index(-1);
        menu.set_deleting_index(-1);
        menu.set_display_mode(display_mode_index(ui.display_mode));
        menu.set_scale_index(scale_pct_to_index(ui.overlay_scale_pct));
        menu.set_timer_enabled(ui.active_profile.is_some());
        menu.set_undo_reset_enabled(ui.can_undo_reset);
        menu.set_cap_calibration(i18n.tr("overlay.menu.calibration").into());
        menu.set_cap_display(i18n.tr("overlay.menu.display").into());
        menu.set_cap_scale(i18n.tr("overlay.menu.scale").into());
        menu.set_cap_timer(i18n.tr("overlay.menu.timer").into());
        menu.set_cap_cancel(i18n.tr("overlay.dialog.cancel").into());
        menu.set_cap_delete(i18n.tr("overlay.dialog.delete.confirm").into());
        menu.set_label_new(i18n.tr("overlay.menu.new_short").into());
        menu.set_about_text(
            i18n.tr_with(
                "overlay.menu.about",
                &[("version", crate::ui_state::VERSION.to_string())],
            )
            .into(),
        );
    }

    /// Rebuild just the profile-row model (at open, and when the list changes).
    fn rebuild_profiles(menu: &RulerMenu, profiles: &[crate::ui_state::ProfileMenuItem]) {
        let rows: Vec<ProfileRow> = profiles
            .iter()
            .map(|profile| ProfileRow {
                name: profile.basename.as_str().into(),
                frames: profile.total_frames_str.as_str().into(),
                active: profile.is_active,
            })
            .collect();
        menu.set_profiles(ModelRc::new(VecModel::from(rows)));
    }

    /// Cheap change-detector for the profile list. When it differs from the menu's
    /// last-seen value the model is rebuilt (and the popup resized if the count
    /// changed), so an inline rename/delete reflects without reopening the menu.
    fn profiles_signature(profiles: &[crate::ui_state::ProfileMenuItem]) -> String {
        let mut sig = String::new();
        for profile in profiles {
            sig.push_str(&profile.filename);
            sig.push('\u{1}');
            sig.push_str(&profile.basename);
            sig.push('\u{1}');
            sig.push_str(&profile.total_frames_str);
            sig.push(if profile.is_active { '1' } else { '0' });
            sig.push('\u{2}');
        }
        sig
    }

    fn wire_menu_callbacks(
        menu: &RulerMenu,
        command_tx: &Sender<UiCommand>,
        state: &Arc<SharedAppState>,
        closing: &Rc<Cell<bool>>,
    ) {
        menu.on_new_profile({
            let tx = command_tx.clone();
            let closing = Rc::clone(closing);
            move || {
                let _ = tx.send(UiCommand::PrepareCalibration);
                closing.set(true);
            }
        });
        menu.on_select_profile({
            let tx = command_tx.clone();
            let state = Arc::clone(state);
            let closing = Rc::clone(closing);
            move |idx| {
                if let Some(profile) = state.snapshot().ui.profiles.get(idx as usize) {
                    let _ = tx.send(UiCommand::UseProfile {
                        filename: profile.filename.clone(),
                    });
                }
                closing.set(true);
            }
        });
        menu.on_commit_rename({
            let tx = command_tx.clone();
            let state = Arc::clone(state);
            move |idx, new_name| {
                let new_base = new_name.trim().to_string();
                if new_base.is_empty() {
                    return;
                }
                if let Some(profile) = state.snapshot().ui.profiles.get(idx as usize) {
                    let _ = tx.send(UiCommand::RenameProfile {
                        old: profile.filename.clone(),
                        new_base,
                    });
                }
            }
        });
        menu.on_confirm_delete({
            let tx = command_tx.clone();
            let state = Arc::clone(state);
            move |idx| {
                if let Some(profile) = state.snapshot().ui.profiles.get(idx as usize) {
                    let _ = tx.send(UiCommand::DeleteProfile {
                        filename: profile.filename.clone(),
                    });
                }
            }
        });
        // Settings actions keep the panel open; the tick re-syncs the selection.
        menu.on_set_display({
            let tx = command_tx.clone();
            move |index| {
                let _ = tx.send(UiCommand::SetDisplayMode(index_to_display_mode(index)));
            }
        });
        menu.on_set_scale({
            let tx = command_tx.clone();
            move |index| {
                let _ = tx.send(UiCommand::SetOverlayScale(index_to_scale(index)));
            }
        });
        menu.on_timer_action({
            let tx = command_tx.clone();
            let state = Arc::clone(state);
            move |action| {
                let cycle = state.snapshot().ui.total_frames_in_cycle;
                let command = match action {
                    0 => UiCommand::AdjustTimer { frames: -cycle },
                    1 => UiCommand::AdjustTimer { frames: -1 },
                    2 => UiCommand::ResetTimer,
                    3 => UiCommand::AdjustTimer { frames: 1 },
                    4 => UiCommand::AdjustTimer { frames: cycle },
                    5 => UiCommand::UndoResetTimer,
                    _ => return,
                };
                let _ = tx.send(command);
            }
        });
        menu.on_about({
            let closing = Rc::clone(closing);
            move || {
                unsafe { menu::win32::open_about_page() };
                closing.set(true);
            }
        });
        menu.on_exit({
            let tx = command_tx.clone();
            let closing = Rc::clone(closing);
            move || {
                let _ = tx.send(UiCommand::Exit);
                closing.set(true);
            }
        });
    }

    // ===================== inline-edit keyboard plumbing =====================

    /// Dispatch a press + release for `text` (a typed character or a `Key` glyph)
    /// to whichever text field currently holds focus (the inline rename field).
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
    /// the (stashed) high surrogate.
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

    unsafe extern "system" fn menu_window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_NCCREATE => {
                let create_struct = lparam.0 as *const CREATESTRUCTW;
                let state_ptr = (*create_struct).lpCreateParams as *mut MenuState;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
                LRESULT(1)
            }
            WM_TIMER => {
                menu_tick(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if let Some(state) = menu_state(hwnd) {
                    let position = logical_pos(lparam, state.scale);
                    let _ = state
                        .window
                        .window()
                        .try_dispatch_event(WindowEvent::PointerMoved { position });
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                if let Some(state) = menu_state(hwnd) {
                    let (x, y) = client_xy(lparam);
                    let inside =
                        x >= 0 && y >= 0 && x < state.buf_w as i32 && y < state.buf_h as i32;
                    if !inside {
                        state.closing.set(true);
                    } else if message == WM_LBUTTONDOWN {
                        let position = logical_pos(lparam, state.scale);
                        let _ =
                            state
                                .window
                                .window()
                                .try_dispatch_event(WindowEvent::PointerPressed {
                                    position,
                                    button: PointerEventButton::Left,
                                });
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Some(state) = menu_state(hwnd) {
                    let (x, y) = client_xy(lparam);
                    let inside =
                        x >= 0 && y >= 0 && x < state.buf_w as i32 && y < state.buf_h as i32;
                    if inside {
                        let position = logical_pos(lparam, state.scale);
                        let _ = state.window.window().try_dispatch_event(
                            WindowEvent::PointerReleased {
                                position,
                                button: PointerEventButton::Left,
                            },
                        );
                    }
                }
                LRESULT(0)
            }
            WM_CHAR => {
                // Inline-rename text entry: forward printable units to the focused
                // field. Control codes (Enter/Esc/Backspace) come via WM_KEYDOWN.
                if let Some(state) = menu_state(hwnd) {
                    if state.menu.get_editing_index() >= 0 {
                        if let Some(text) = decode_wm_char(&state.pending_high, wparam.0 as u16) {
                            dispatch_key(&state.window, text);
                        }
                    }
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if let Some(state) = menu_state(hwnd) {
                    let editing = state.menu.get_editing_index() >= 0;
                    let deleting = state.menu.get_deleting_index() >= 0;
                    let vk = wparam.0 as u16;
                    if vk == VK_ESCAPE.0 {
                        if editing || deleting {
                            // Esc backs out of an inline edit / confirm first.
                            state.menu.set_editing_index(-1);
                            state.menu.set_deleting_index(-1);
                        } else {
                            state.closing.set(true);
                        }
                    } else if editing {
                        if vk == VK_RETURN.0 {
                            // Let the focused field's `accepted` commit the rename.
                            dispatch_key(&state.window, Key::Return.into());
                        } else {
                            let key = match vk {
                                v if v == VK_BACK.0 => Some(Key::Backspace),
                                v if v == VK_DELETE.0 => Some(Key::Delete),
                                v if v == VK_LEFT.0 => Some(Key::LeftArrow),
                                v if v == VK_RIGHT.0 => Some(Key::RightArrow),
                                v if v == VK_HOME.0 => Some(Key::Home),
                                v if v == VK_END.0 => Some(Key::End),
                                _ => None,
                            };
                            if let Some(key) = key {
                                dispatch_key(&state.window, key.into());
                            }
                        }
                    }
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MenuState;
                if !state_ptr.is_null() {
                    let _ = Box::from_raw(state_ptr);
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn menu_tick(hwnd: HWND) {
        slint::platform::update_timers_and_animations();
        let Some(state) = menu_state_mut(hwnd) else {
            return;
        };
        let snapshot = state.state.snapshot();
        // Live-sync the cheap selections so chips reflect changes made via the
        // menu (and the worker) without rebuilding the profile model each frame.
        state
            .menu
            .set_display_mode(display_mode_index(snapshot.ui.display_mode));
        state
            .menu
            .set_scale_index(scale_pct_to_index(snapshot.ui.overlay_scale_pct));
        state
            .menu
            .set_timer_enabled(snapshot.ui.active_profile.is_some());
        state
            .menu
            .set_undo_reset_enabled(snapshot.ui.can_undo_reset);

        // Refresh the profile model only when the list actually changes (an inline
        // rename/delete landed), so an in-progress edit field is not torn down each
        // frame. Resize the popup when the row count changed.
        let sig = profiles_signature(&snapshot.ui.profiles);
        if state.profiles_sig != sig {
            rebuild_profiles(&state.menu, &snapshot.ui.profiles);
            state.profiles_sig = sig;
            let new_count = snapshot.ui.profiles.len();
            if new_count != state.profile_count {
                state.profile_count = new_count;
                resize_menu(hwnd, state, new_count);
            }
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

    /// Resize the open menu popup to fit `n_profiles` rows (after an inline delete
    /// removed one). Rebuilds the framebuffer DIB and the native + Slint window
    /// size, keeping the top-left corner fixed.
    unsafe fn resize_menu(hwnd: HWND, state: &mut MenuState, n_profiles: usize) {
        let width = state.buf_w as i32;
        let height = (menu_logical_height(n_profiles) * state.scale).round() as i32;
        let _ = DeleteDC(state.mem_dc);
        let _ = DeleteObject(HGDIOBJ(state.dib.0));
        if let Some((mem_dc, dib, bits)) = create_dib(width, height) {
            state.mem_dc = mem_dc;
            state.dib = dib;
            state.bits = bits;
            state.buf_h = height as usize;
        }
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

    unsafe fn menu_state<'a>(hwnd: HWND) -> Option<&'a MenuState> {
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MenuState;
        if state_ptr.is_null() {
            None
        } else {
            Some(&*state_ptr)
        }
    }

    unsafe fn menu_state_mut<'a>(hwnd: HWND) -> Option<&'a mut MenuState> {
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MenuState;
        if state_ptr.is_null() {
            None
        } else {
            Some(&mut *state_ptr)
        }
    }

    unsafe fn should_exit(hwnd: HWND) -> bool {
        window_state(hwnd)
            .map(|state| state.state.snapshot().ui.should_exit)
            .unwrap_or(false)
    }

    unsafe fn window_state<'a>(hwnd: HWND) -> Option<&'a WindowState> {
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
        if state_ptr.is_null() {
            None
        } else {
            Some(&*state_ptr)
        }
    }

    unsafe fn window_state_mut<'a>(hwnd: HWND) -> Option<&'a mut WindowState> {
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
        if state_ptr.is_null() {
            None
        } else {
            Some(&mut *state_ptr)
        }
    }

    unsafe fn outer_area_should_pass_through(hwnd: HWND, state: &mut WindowState) -> bool {
        let mut cursor = POINT::default();
        if GetCursorPos(&mut cursor).is_err() {
            return false;
        }
        let mut window_rect = RECT::default();
        if GetWindowRect(hwnd, &mut window_rect).is_err() {
            return false;
        }

        let x = cursor.x - window_rect.left;
        let y = cursor.y - window_rect.top;
        let running = matches!(state.state.snapshot().ui.mode, OverlayMode::Running);
        outer_area_should_pass_through_at(x, y, state.scale, running)
    }

    fn outer_area_should_pass_through_at(x: i32, y: i32, scale: f32, running: bool) -> bool {
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
    fn initial_geometry(pos: Option<(i32, i32)>, scale_mult: f32) -> (i32, i32, i32, i32, f32) {
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
    }
}
