use std::{fmt, sync::Arc};

use crate::worker::SharedAppState;

#[derive(Debug, Clone)]
pub struct OverlaySpec {
    pub title: String,
    pub body_text: String,
    pub width: i32,
    pub height: i32,
}

impl Default for OverlaySpec {
    fn default() -> Self {
        Self {
            title: "Arknights Cost Bar Ruler".to_string(),
            body_text: "Overlay bootstrap ready\nFrame: --\nTimer: 00:00.000".to_string(),
            width: 320,
            height: 120,
        }
    }
}

pub struct OverlayRuntime {
    spec: OverlaySpec,
    state: Arc<SharedAppState>,
}

impl fmt::Debug for OverlayRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OverlayRuntime")
            .field("spec", &self.spec)
            .field("state", &self.state.snapshot())
            .finish()
    }
}

impl OverlayRuntime {
    #[must_use]
    pub fn new(spec: OverlaySpec, state: Arc<SharedAppState>) -> Self {
        Self { spec, state }
    }

    #[must_use]
    pub fn startup_note(&self) -> String {
        let snapshot = self.state.snapshot();
        format!(
            "native overlay runtime registered with {} / {} / {}",
            snapshot.status_text, snapshot.frame_text, snapshot.timer_text
        )
    }

    pub fn run(&self) -> Result<(), OverlayError> {
        platform::run(&self.spec, Arc::clone(&self.state))
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
    use std::sync::Arc;

    use crate::worker::SharedAppState;

    use super::{OverlayError, OverlaySpec};

    pub fn run(_: &OverlaySpec, _: Arc<SharedAppState>) -> Result<(), OverlayError> {
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
            Arc,
        },
        time::Instant,
    };

    use crate::worker::SharedAppState;
    use super::{OverlayError, OverlaySpec};
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
            Graphics::Gdi::{
                BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
                InvalidateRect, SetBkMode, SetTextColor, HBRUSH, HDC, PAINTSTRUCT, TRANSPARENT,
                DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK,
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                CreateWindowExW, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
                CW_USEDEFAULT, DefWindowProcW, DispatchMessageW, GetMessageW,
                GetClientRect, GetWindowLongPtrW, HMENU, IDC_ARROW, LoadCursorW, MSG, PostQuitMessage,
                RegisterClassW, SW_SHOW, SetWindowLongPtrW, ShowWindow, TranslateMessage,
                SetLayeredWindowAttributes, SetTimer, LWA_ALPHA, WINDOW_EX_STYLE, WINDOW_STYLE,
                WM_APP, WM_DESTROY, WM_NCCREATE, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
                WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
                GWLP_USERDATA, PostMessageW,
            },
        },
    };

    const OVERLAY_ALPHA: u8 = 224;
    const OVERLAY_TIMER_ID: usize = 1;
    const OVERLAY_TIMER_INTERVAL_MS: u32 = 16;
    const WM_OVERLAY_WAKE: u32 = WM_APP + 2;

    struct WindowState {
        spec: OverlaySpec,
        state: Arc<SharedAppState>,
        background_brush: HBRUSH,
    }

    impl Drop for WindowState {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteObject(self.background_brush);
            }
        }
    }

    pub fn run(spec: &OverlaySpec, shared_state: Arc<SharedAppState>) -> Result<(), OverlayError> {
        unsafe {
            let instance = GetModuleHandleW(PCWSTR::null())
                .map_err(|error| OverlayError::new(format!("GetModuleHandleW failed: {error}")))?;

            let class_name = wide("RulerOverlayWindowClass");
            let title = wide(&spec.title);

            let background_brush = CreateSolidBrush(COLORREF(0x00202020));
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
                ..Default::default()
            };

            let atom = RegisterClassW(&class);
            if atom == 0 {
                return Err(OverlayError::new("RegisterClassW failed"));
            }

            let state = Box::new(WindowState {
                spec: spec.clone(),
                state: Arc::clone(&shared_state),
                background_brush,
            });
            let state_ptr = Box::into_raw(state);

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_LAYERED.0),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WINDOW_STYLE(WS_POPUP.0 | WS_VISIBLE.0),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                spec.width,
                spec.height,
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
                    let _ = PostMessageW(HWND(raw_hwnd as *mut _), WM_OVERLAY_WAKE, WPARAM(0), LPARAM(0));
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
                let create_struct = lparam.0
                    as *const CREATESTRUCTW;
                let state_ptr = (*create_struct).lpCreateParams as *mut WindowState;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
                LRESULT(1)
            }
            WM_PAINT => {
                paint_window(hwnd);
                LRESULT(0)
            }
            WM_OVERLAY_WAKE => {
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_TIMER => {
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_DESTROY => {
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
                if !state_ptr.is_null() {
                    (*state_ptr).state.set_overlay_waker(None);
                }
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
                if !state_ptr.is_null() {
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
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;

        if state_ptr.is_null() {
            return;
        }

        let state = &*state_ptr;
        let mut paint = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut paint);
        let mut client_rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut client_rect);

        FillRect(hdc, &client_rect, state.background_brush);
        draw_overlay_text(hdc, &client_rect, state);
        state
            .state
            .record_overlay_paint(state.state.snapshot().worker_timing.sample_index, Instant::now());

        let _ = EndPaint(hwnd, &paint);
    }

    unsafe fn draw_overlay_text(hdc: HDC, client_rect: &RECT, state: &WindowState) {
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0x00F0F0F0));
        let snapshot = state.state.snapshot();

        let mut title_rect = RECT {
            left: client_rect.left + 16,
            top: client_rect.top + 12,
            right: client_rect.right - 16,
            bottom: client_rect.top + 40,
        };
        let mut title = wide(&state.spec.title);
        DrawTextW(
            hdc,
            title.as_mut_slice(),
            &mut title_rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

        let mut body_rect = RECT {
            left: client_rect.left + 16,
            top: client_rect.top + 44,
            right: client_rect.right - 16,
            bottom: client_rect.bottom - 12,
        };
        let mut body = wide(&format!(
            "{}\n{}\n{}\n{}\n{}",
            state.spec.body_text,
            snapshot.status_text,
            snapshot.frame_text,
            snapshot.timer_text,
            snapshot.latency_text
        ));
        DrawTextW(
            hdc,
            body.as_mut_slice(),
            &mut body_rect,
            DT_CENTER | DT_VCENTER | DT_WORDBREAK | DT_NOPREFIX,
        );
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(iter::once(0)).collect()
    }
}
