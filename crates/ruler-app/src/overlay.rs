use std::{
    fmt,
    sync::{mpsc::Sender, Arc},
};

use crate::{commands::UiCommand, i18n::I18n, icons::IconSet, worker::SharedAppState};

pub struct OverlayRuntime {
    state: Arc<SharedAppState>,
    command_tx: Sender<UiCommand>,
    i18n: Arc<I18n>,
    icons: Arc<IconSet>,
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
    ) -> Self {
        Self {
            state,
            command_tx,
            i18n,
            icons,
        }
    }

    #[must_use]
    pub fn startup_note(&self) -> String {
        let snapshot = self.state.snapshot();
        format!(
            "native overlay runtime registered with mode={:?}",
            snapshot.ui.mode
        )
    }

    pub fn run(&self) -> Result<(), OverlayError> {
        platform::run(
            Arc::clone(&self.state),
            self.command_tx.clone(),
            Arc::clone(&self.i18n),
            Arc::clone(&self.icons),
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
    ) -> Result<(), OverlayError> {
        Err(OverlayError::new(
            "native overlay window is currently implemented for Windows only",
        ))
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        iter,
        sync::{
            atomic::{AtomicIsize, Ordering},
            mpsc::Sender,
            Arc,
        },
        time::Instant,
    };

    use ruler_core::analysis::roi::find_cost_bar_roi;

    use super::OverlayError;
    use crate::{
        commands::UiCommand,
        i18n::I18n,
        icons::{win32::draw_scaled, IconSet},
        menu,
        ui_state::OverlayMode,
        worker::SharedAppState,
    };
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
            Graphics::Gdi::{
                BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint,
                FillRect, GetStockObject, InvalidateRect, SelectObject, SetBkMode, SetTextColor,
                DRAW_TEXT_FORMAT, DT_CENTER, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
                DT_WORDBREAK, HBRUSH, HDC, HGDIOBJ, PAINTSTRUCT, TRANSPARENT, WHITE_BRUSH,
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                Input::KeyboardAndMouse::{ReleaseCapture, SetCapture},
                WindowsAndMessaging::{
                    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                    GetClientRect, GetCursorPos, GetMessageW, GetSystemMetrics, GetWindowLongPtrW,
                    GetWindowRect, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassW,
                    SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos,
                    ShowWindow, TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
                    GWLP_USERDATA, HMENU, IDC_ARROW, LWA_ALPHA, MSG, SM_CXSCREEN, SM_CYSCREEN,
                    SWP_NOSIZE, SWP_NOZORDER, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
                    WM_COMMAND, WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
                    WM_NCCREATE, WM_PAINT, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
                    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
                },
            },
        },
    };

    const OVERLAY_ALPHA: u8 = 191;
    const OVERLAY_TIMER_ID: usize = 1;
    const OVERLAY_TIMER_INTERVAL_MS: u32 = 16;
    const WM_OVERLAY_WAKE: u32 = WM_APP + 2;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum HitTarget {
        None,
        StartButton,
        Timer,
    }

    struct WindowState {
        state: Arc<SharedAppState>,
        command_tx: Sender<UiCommand>,
        i18n: Arc<I18n>,
        icons: Arc<IconSet>,
        background_brush: HBRUSH,
        mouse_down: bool,
        moved: bool,
        hit_target: HitTarget,
        drag_origin: POINT,
        window_origin: POINT,
    }

    impl Drop for WindowState {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteObject(self.background_brush);
            }
        }
    }

    pub fn run(
        shared_state: Arc<SharedAppState>,
        command_tx: Sender<UiCommand>,
        i18n: Arc<I18n>,
        icons: Arc<IconSet>,
    ) -> Result<(), OverlayError> {
        unsafe {
            let instance = GetModuleHandleW(PCWSTR::null())
                .map_err(|error| OverlayError::new(format!("GetModuleHandleW failed: {error}")))?;

            let class_name = wide("RulerOverlayWindowClass");
            let title = wide("Arknights Cost Bar Ruler");
            let background_brush = CreateSolidBrush(COLORREF(0x003a3a3a));
            if background_brush.0.is_null() {
                return Err(OverlayError::new("CreateSolidBrush failed"));
            }

            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                hInstance: HINSTANCE(instance.0),
                lpszClassName: PCWSTR(class_name.as_ptr()),
                hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW)
                    .map_err(|error| OverlayError::new(format!("LoadCursorW failed: {error}")))?,
                hbrBackground: HBRUSH(GetStockObject(WHITE_BRUSH).0),
                ..Default::default()
            };

            let atom = RegisterClassW(&class);
            if atom == 0 {
                return Err(OverlayError::new("RegisterClassW failed"));
            }

            let geometry = initial_geometry();
            let state = Box::new(WindowState {
                state: Arc::clone(&shared_state),
                command_tx,
                i18n,
                icons,
                background_brush,
                mouse_down: false,
                moved: false,
                hit_target: HitTarget::None,
                drag_origin: POINT::default(),
                window_origin: POINT::default(),
            });
            let state_ptr = Box::into_raw(state);

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_LAYERED.0 | WS_EX_TOOLWINDOW.0),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WINDOW_STYLE(WS_POPUP.0 | WS_VISIBLE.0),
                geometry.left,
                geometry.top,
                geometry.right - geometry.left,
                geometry.bottom - geometry.top,
                HWND::default(),
                HMENU::default(),
                HINSTANCE(instance.0),
                Some(state_ptr.cast()),
            )
            .map_err(|error| OverlayError::new(format!("CreateWindowExW failed: {error}")))?;

            if hwnd.0.is_null() {
                let _ = Box::from_raw(state_ptr);
                return Err(OverlayError::new("CreateWindowExW failed"));
            }

            SetLayeredWindowAttributes(hwnd, COLORREF(0), OVERLAY_ALPHA, LWA_ALPHA).map_err(
                |error| OverlayError::new(format!("SetLayeredWindowAttributes failed: {error}")),
            )?;

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
                paint_window(hwnd);
                LRESULT(0)
            }
            WM_OVERLAY_WAKE | WM_TIMER => {
                if should_exit(hwnd) {
                    let _ = DestroyWindow(hwnd);
                } else {
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                handle_left_down(hwnd, lparam);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                handle_mouse_move(hwnd);
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

    unsafe fn paint_window(hwnd: HWND) {
        let Some(state) = window_state(hwnd) else {
            return;
        };

        let mut paint = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut paint);
        let mut client_rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut client_rect);

        FillRect(hdc, &client_rect, state.background_brush);
        draw_overlay(hdc, &client_rect, state);
        state.state.record_overlay_paint(
            state.state.snapshot().worker_timing.sample_index,
            Instant::now(),
        );

        let _ = EndPaint(hwnd, &paint);
    }

    unsafe fn draw_overlay(hdc: HDC, rect: &RECT, state: &WindowState) {
        SetBkMode(hdc, TRANSPARENT);
        let snapshot = state.state.snapshot();
        let layout = Layout::new(*rect, state.i18n.locale());

        match snapshot.ui.mode {
            OverlayMode::Idle => {
                draw_main_icon(hdc, &state.icons, "deco", layout.icon_rect);
                draw_center_text(
                    hdc,
                    layout.right_rect,
                    &state.i18n.tr("overlay.msg.idle"),
                    layout.medium_font,
                    false,
                );
            }
            OverlayMode::PreCalibration => {
                draw_main_icon(hdc, &state.icons, "start", layout.icon_rect);
                draw_center_text(
                    hdc,
                    layout.right_rect,
                    &state.i18n.tr("overlay.msg.pre_cal"),
                    layout.medium_font,
                    false,
                );
            }
            OverlayMode::Calibrating => {
                draw_main_icon(hdc, &state.icons, "wait", layout.icon_rect);
                draw_center_text(
                    hdc,
                    layout.right_rect,
                    &format!("{}%", snapshot.ui.progress_percent),
                    layout.large_font,
                    false,
                );
            }
            OverlayMode::Running => {
                draw_main_icon(hdc, &state.icons, "deco", layout.icon_rect);
                draw_right_text(
                    hdc,
                    layout.frame_rect,
                    &snapshot.ui.display_frame,
                    layout.large_font,
                    true,
                    COLORREF(0x00ffffff),
                );
                draw_right_text(
                    hdc,
                    layout.total_rect,
                    &snapshot.ui.display_total,
                    layout.medium_font,
                    false,
                    COLORREF(0x00999999),
                );
                draw_timer(
                    hdc,
                    &state.icons,
                    &layout.timer_rect,
                    &snapshot.ui.time_str,
                    layout.small_font,
                );
                if let Some(lap_frames) = snapshot.ui.lap_frames {
                    draw_lap(
                        hdc,
                        &state.icons,
                        &layout.lap_rect,
                        &lap_frames.to_string(),
                        layout.small_font,
                    );
                }
            }
            OverlayMode::Error => {
                draw_main_icon(hdc, &state.icons, "deco", layout.icon_rect);
                let message = format!("错误:\n{}", truncate(&snapshot.ui.message, 50));
                draw_center_text(
                    hdc,
                    layout.right_rect,
                    &message,
                    layout.small_font.max(14),
                    false,
                );
            }
            OverlayMode::Booting => {
                draw_main_icon(hdc, &state.icons, "deco", layout.icon_rect);
                draw_center_text(
                    hdc,
                    layout.right_rect,
                    &snapshot.ui.message,
                    layout.small_font.max(14),
                    false,
                );
            }
        }
    }

    unsafe fn draw_main_icon(hdc: HDC, icons: &IconSet, name: &str, rect: RECT) {
        if let Some(icon) = icons.get(name) {
            draw_scaled(hdc, icon, rect, 255);
        }
    }

    unsafe fn draw_timer(hdc: HDC, icons: &IconSet, rect: &RECT, text: &str, font_height: i32) {
        let icon_size = (rect.bottom - rect.top).max(1);
        let icon_rect = RECT {
            left: rect.left,
            top: rect.top,
            right: rect.left + icon_size,
            bottom: rect.bottom,
        };
        if let Some(icon) = icons.get("timer") {
            draw_scaled(hdc, icon, icon_rect, 255);
        }
        let text_rect = RECT {
            left: rect.left + icon_size + 2,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        draw_left_text(
            hdc,
            text_rect,
            text,
            font_height,
            false,
            COLORREF(0x00999999),
        );
    }

    unsafe fn draw_lap(hdc: HDC, icons: &IconSet, rect: &RECT, text: &str, font_height: i32) {
        let icon_size = (rect.bottom - rect.top).max(1);
        let icon_rect = RECT {
            left: rect.left,
            top: rect.top,
            right: rect.left + icon_size,
            bottom: rect.bottom,
        };
        if let Some(icon) = icons.get("wait") {
            draw_scaled(hdc, icon, icon_rect, 255);
        }
        let text_rect = RECT {
            left: rect.left + icon_size + 2,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        draw_left_text(
            hdc,
            text_rect,
            text,
            font_height,
            false,
            COLORREF(0x00999999),
        );
    }

    unsafe fn draw_center_text(hdc: HDC, rect: RECT, text: &str, font_height: i32, bold: bool) {
        draw_text(
            hdc,
            rect,
            text,
            font_height,
            bold,
            COLORREF(0x00ffffff),
            DT_CENTER | DT_VCENTER | DT_WORDBREAK | DT_NOPREFIX,
        );
    }

    unsafe fn draw_left_text(
        hdc: HDC,
        rect: RECT,
        text: &str,
        font_height: i32,
        bold: bool,
        color: COLORREF,
    ) {
        draw_text(
            hdc,
            rect,
            text,
            font_height,
            bold,
            color,
            DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
    }

    unsafe fn draw_right_text(
        hdc: HDC,
        rect: RECT,
        text: &str,
        font_height: i32,
        bold: bool,
        color: COLORREF,
    ) {
        draw_text(
            hdc,
            rect,
            text,
            font_height,
            bold,
            color,
            DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
    }

    unsafe fn draw_text(
        hdc: HDC,
        mut rect: RECT,
        text: &str,
        font_height: i32,
        bold: bool,
        color: COLORREF,
        flags: DRAW_TEXT_FORMAT,
    ) {
        SetTextColor(hdc, color);
        let face = wide("Segoe UI");
        let font = CreateFontW(
            -font_height,
            0,
            0,
            0,
            if bold { 700 } else { 400 },
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            PCWSTR(face.as_ptr()),
        );
        let old_font = if font.0.is_null() {
            HGDIOBJ::default()
        } else {
            SelectObject(hdc, HGDIOBJ(font.0))
        };
        let mut text = wide(text);
        DrawTextW(hdc, text.as_mut_slice(), &mut rect, flags);
        if !font.0.is_null() {
            let _ = SelectObject(hdc, old_font);
            let _ = DeleteObject(font);
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
        let mut client_rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut client_rect);
        let point = POINT {
            x: get_x_lparam(lparam),
            y: get_y_lparam(lparam),
        };
        state.mouse_down = true;
        state.moved = false;
        state.hit_target = hit_test(
            &state.state.snapshot().ui.mode,
            &Layout::new(client_rect, "zh_CN"),
            point,
        );
        state.drag_origin = cursor;
        state.window_origin = POINT {
            x: window_rect.left,
            y: window_rect.top,
        };
        let _ = SetCapture(hwnd);
    }

    unsafe fn handle_mouse_move(hwnd: HWND) {
        let Some(state) = window_state_mut(hwnd) else {
            return;
        };
        if !state.mouse_down {
            return;
        }
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
        }
    }

    unsafe fn handle_left_up(hwnd: HWND) {
        let Some(state) = window_state_mut(hwnd) else {
            return;
        };
        let hit_target = state.hit_target;
        let moved = state.moved;
        state.mouse_down = false;
        state.moved = false;
        state.hit_target = HitTarget::None;
        let _ = ReleaseCapture();

        if moved {
            return;
        }

        match hit_target {
            HitTarget::StartButton => {
                let _ = state.command_tx.send(UiCommand::StartCalibration);
            }
            HitTarget::Timer => {
                let _ = state.command_tx.send(UiCommand::ToggleLapTimer);
            }
            HitTarget::None => {}
        }
    }

    fn hit_test(mode: &OverlayMode, layout: &Layout, point: POINT) -> HitTarget {
        if matches!(mode, OverlayMode::PreCalibration) && contains(layout.left_rect, point) {
            return HitTarget::StartButton;
        }
        if matches!(mode, OverlayMode::Running) && contains(layout.timer_hit_rect, point) {
            return HitTarget::Timer;
        }
        HitTarget::None
    }

    fn contains(rect: RECT, point: POINT) -> bool {
        point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
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

    #[derive(Clone, Copy)]
    struct Layout {
        left_rect: RECT,
        right_rect: RECT,
        icon_rect: RECT,
        frame_rect: RECT,
        total_rect: RECT,
        timer_rect: RECT,
        timer_hit_rect: RECT,
        lap_rect: RECT,
        large_font: i32,
        medium_font: i32,
        small_font: i32,
    }

    impl Layout {
        fn new(client: RECT, locale: &str) -> Self {
            let width = client.right - client.left;
            let height = client.bottom - client.top;
            let left_width = width * 33 / 100;
            let right_rect = RECT {
                left: left_width,
                top: 0,
                right: width,
                bottom: height,
            };
            let left_rect = RECT {
                left: 0,
                top: 0,
                right: left_width,
                bottom: height,
            };
            let icon_size = left_width.min(height).max(1);
            let icon_rect = RECT {
                left: (left_width - icon_size) / 2,
                top: (height - icon_size) / 2,
                right: (left_width + icon_size) / 2,
                bottom: (height + icon_size) / 2,
            };
            let padding = (height / 100).max(2);
            let offset_x = width * 20 / 100;
            let small_font = (height * 18 / 100).max(10);
            let medium_font = if locale == "en_US" {
                (height * 18 / 100).max(12)
            } else {
                (height * 22 / 100).max(12)
            };
            let large_font = (height * 55 / 100).max(20);
            let timer_height = small_font + 4;
            let timer_rect = RECT {
                left: padding,
                top: height - timer_height - padding,
                right: left_width + width / 5,
                bottom: height - padding,
            };
            let lap_rect = RECT {
                left: padding,
                top: padding,
                right: left_width + width / 5,
                bottom: padding + timer_height,
            };
            Self {
                left_rect,
                right_rect,
                icon_rect,
                frame_rect: RECT {
                    left: left_width,
                    top: height * 18 / 100,
                    right: width - offset_x,
                    bottom: height * 65 / 100,
                },
                total_rect: RECT {
                    left: left_width,
                    top: height - medium_font - padding * 2,
                    right: width - padding,
                    bottom: height - padding,
                },
                timer_hit_rect: timer_rect,
                timer_rect,
                lap_rect,
                large_font,
                medium_font,
                small_font,
            }
        }
    }

    fn initial_geometry() -> RECT {
        unsafe {
            let screen_width = GetSystemMetrics(SM_CXSCREEN);
            let screen_height = GetSystemMetrics(SM_CYSCREEN);
            let (roi_x1, roi_x2, _) = find_cost_bar_roi(screen_width, screen_height);
            let cost_bar_pixel_length = (roi_x2 - roi_x1).abs().max(180);
            let width = cost_bar_pixel_length * 5 / 6;
            let height = width * 27 / 50;
            let left = screen_width - width - 50;
            let top = screen_height - height - 100;
            RECT {
                left,
                top,
                right: left + width,
                bottom: top + height,
            }
        }
    }

    fn get_x_lparam(lparam: LPARAM) -> i32 {
        (lparam.0 as u32 & 0xffff) as i16 as i32
    }

    fn get_y_lparam(lparam: LPARAM) -> i32 {
        ((lparam.0 as u32 >> 16) & 0xffff) as i16 as i32
    }

    fn truncate(value: &str, max_chars: usize) -> String {
        if value.chars().count() <= max_chars {
            value.to_string()
        } else {
            format!("{}...", value.chars().take(max_chars).collect::<String>())
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(iter::once(0)).collect()
    }
}
