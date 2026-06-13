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
        ffi::c_void,
        iter,
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
            software_renderer::{
                MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, SoftwareRenderer,
                TargetPixel,
            },
            Platform, PointerEventButton, WindowAdapter, WindowEvent,
        },
        ComponentHandle, LogicalPosition, PhysicalSize, PlatformError,
    };

    use super::OverlayError;
    use crate::{
        commands::UiCommand,
        i18n::I18n,
        icons::IconSet,
        menu,
        ui::{Hud, HudMode},
        ui_state::OverlayMode,
        worker::SharedAppState,
    };
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
            Graphics::Gdi::{
                CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
                SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
                BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                Controls::WM_MOUSELEAVE,
                Input::KeyboardAndMouse::{
                    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
                },
                WindowsAndMessaging::{
                    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                    GetCursorPos, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
                    LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassW, SetTimer,
                    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, UpdateLayeredWindow,
                    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HMENU, IDC_ARROW, MSG,
                    SM_CXSCREEN, SM_CYSCREEN, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_SHOW,
                    ULW_ALPHA, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_COMMAND, WM_DESTROY,
                    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT, WM_RBUTTONUP,
                    WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
                    WS_VISIBLE,
                },
            },
        },
    };

    const OVERLAY_TIMER_ID: usize = 1;
    const OVERLAY_TIMER_INTERVAL_MS: u32 = 16;
    const WM_OVERLAY_WAKE: u32 = WM_APP + 2;

    // Fixed logical design size of `hud.slint`. Physical size = logical * scale.
    const LOGICAL_W: f32 = 232.0;
    const LOGICAL_H: f32 = 56.0;

    /// Premultiplied BGRA pixel, the exact layout `UpdateLayeredWindow` expects
    /// for a per-pixel-alpha layered window (32bpp top-down DIB, AC_SRC_ALPHA).
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PreBgra {
        b: u8,
        g: u8,
        r: u8,
        a: u8,
    }

    impl TargetPixel for PreBgra {
        fn blend(&mut self, color: PremultipliedRgbaColor) {
            let inv = (u8::MAX - color.alpha) as u16;
            self.r = (self.r as u16 * inv / 255) as u8 + color.red;
            self.g = (self.g as u16 * inv / 255) as u8 + color.green;
            self.b = (self.b as u16 * inv / 255) as u8 + color.blue;
            self.a = (self.a as u16 * inv / 255) as u8 + color.alpha;
        }

        fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
            Self {
                b: blue,
                g: green,
                r: red,
                a: 255,
            }
        }

        // Uncovered / transparent areas of the (transparent) Slint window.
        fn background() -> Self {
            Self {
                b: 0,
                g: 0,
                r: 0,
                a: 0,
            }
        }
    }

    struct RulerPlatform {
        window: Rc<MinimalSoftwareWindow>,
        start: Instant,
    }

    impl Platform for RulerPlatform {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
            Ok(self.window.clone())
        }

        fn duration_since_start(&self) -> core::time::Duration {
            self.start.elapsed()
        }
    }

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
        _icons: Arc<IconSet>,
        placement: super::OverlayPlacement,
    ) -> Result<(), OverlayError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        slint::platform::set_platform(Box::new(RulerPlatform {
            window: window.clone(),
            start: Instant::now(),
        }))
        .map_err(|error| OverlayError::new(format!("set_platform failed: {error:?}")))?;

        let hud = Hud::new()
            .map_err(|error| OverlayError::new(format!("failed to build HUD component: {error}")))?;

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
        wire_callbacks(
            &hud,
            &shared_state,
            &command_tx,
            &drag_on_bg,
            &hwnd_cell,
        );

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
            WM_OVERLAY_WAKE | WM_TIMER => {
                if should_exit(hwnd) {
                    let _ = DestroyWindow(hwnd);
                } else {
                    tick(hwnd);
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
                if let Some(state) = window_state(hwnd) {
                    menu::win32::show_context_menu(hwnd, &state.state, &state.i18n);
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                if let Some(state) = window_state(hwnd) {
                    menu::win32::handle_menu_command(
                        hwnd,
                        wparam.0 & 0xffff,
                        &state.state,
                        &state.command_tx,
                        &state.i18n,
                    );
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
                if !state_ptr.is_null() {
                    (*state_ptr).state.set_overlay_waker(None);
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
        state.window.set_size(PhysicalSize::new(width as u32, height as u32));
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
        let position = LogicalPosition::new(client.x as f32 / state.scale, client.y as f32 / state.scale);

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

    fn logical_pos(lparam: LPARAM, scale: f32) -> LogicalPosition {
        let x = (lparam.0 as u32 & 0xffff) as i16 as f32;
        let y = ((lparam.0 as u32 >> 16) & 0xffff) as i16 as f32;
        LogicalPosition::new(x / scale, y / scale)
    }

    unsafe fn create_dib(width: i32, height: i32) -> Option<(HDC, HBITMAP, *mut PreBgra)> {
        let screen = GetDC(HWND::default());
        let mem_dc = CreateCompatibleDC(screen);
        let _ = ReleaseDC(HWND::default(), screen);
        if mem_dc.0.is_null() {
            return None;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down to match Slint's row order
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let Ok(dib) = CreateDIBSection(mem_dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(mem_dc);
            return None;
        };
        if dib.0.is_null() || bits.is_null() {
            let _ = DeleteDC(mem_dc);
            return None;
        }
        SelectObject(mem_dc, HGDIOBJ(dib.0));
        Some((mem_dc, dib, bits.cast::<PreBgra>()))
    }

    unsafe fn present_layered(hwnd: HWND, mem_dc: HDC, width: i32, height: i32) {
        let screen = GetDC(HWND::default());
        let size = SIZE {
            cx: width,
            cy: height,
        };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            screen,
            None,
            Some(&size as *const SIZE),
            mem_dc,
            Some(&src as *const POINT),
            COLORREF(0),
            Some(&blend as *const BLENDFUNCTION),
            ULW_ALPHA,
        );
        let _ = ReleaseDC(HWND::default(), screen);
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

    /// Returns `(left, top, width, height, base_scale)` in physical pixels.
    /// `base_scale` aligns the bar height to the legacy overlay footprint; the
    /// effective scale is `base_scale * scale_mult`. A persisted `pos` is used
    /// when present (clamped on-screen), otherwise it anchors bottom-right.
    fn initial_geometry(
        pos: Option<(i32, i32)>,
        scale_mult: f32,
    ) -> (i32, i32, i32, i32, f32) {
        unsafe {
            let screen_width = GetSystemMetrics(SM_CXSCREEN);
            let screen_height = GetSystemMetrics(SM_CYSCREEN);
            let (roi_x1, roi_x2, _) = find_cost_bar_roi(screen_width, screen_height);
            let cost_bar_pixel_length = (roi_x2 - roi_x1).abs().max(180);
            let legacy_height = (cost_bar_pixel_length * 5 / 6) * 27 / 50;
            let base_scale = (legacy_height as f32 / LOGICAL_H).clamp(1.0, 4.0);
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

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(iter::once(0)).collect()
    }
}
