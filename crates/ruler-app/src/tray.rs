use std::{fmt, sync::Arc};

use crate::worker::SharedAppState;

#[derive(Debug)]
pub struct TrayRuntime {
    state: Arc<SharedAppState>,
}

impl TrayRuntime {
    #[must_use]
    pub fn new(state: Arc<SharedAppState>) -> Self {
        Self { state }
    }

    #[must_use]
    pub fn startup_note(&self) -> String {
        let snapshot = self.state.snapshot();
        format!(
            "native tray runtime ready with menu state from {} / {} / {}",
            snapshot.status_text, snapshot.frame_text, snapshot.timer_text
        )
    }

    pub fn run(&self) -> Result<TrayHandle, TrayError> {
        platform::run(Arc::clone(&self.state))
    }
}

#[derive(Debug)]
pub struct TrayHandle {
    inner: platform::TrayHandleImpl,
}

impl TrayHandle {
    fn new(inner: platform::TrayHandleImpl) -> Self {
        Self { inner }
    }
}

impl Drop for TrayHandle {
    fn drop(&mut self) {
        self.inner.shutdown();
    }
}

#[derive(Debug)]
pub struct TrayError {
    message: String,
}

impl TrayError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TrayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TrayError {}

#[cfg(not(windows))]
mod platform {
    use std::sync::Arc;

    use crate::worker::SharedAppState;

    use super::{TrayError, TrayHandle};

    #[derive(Debug)]
    pub struct TrayHandleImpl;

    impl TrayHandleImpl {
        pub fn shutdown(&mut self) {}
    }

    pub fn run(_: Arc<SharedAppState>) -> Result<TrayHandle, TrayError> {
        Ok(TrayHandle::new(TrayHandleImpl))
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        iter,
        mem,
        sync::{
            mpsc::{self, Receiver, Sender},
            Arc,
        },
        thread::{self, JoinHandle},
    };

    use crate::worker::SharedAppState;
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
            Graphics::Gdi::HBRUSH,
            System::{LibraryLoader::GetModuleHandleW, Threading::ExitProcess},
            UI::{
                Shell::{
                    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
                    NIM_MODIFY, NOTIFYICONDATAW,
                },
                WindowsAndMessaging::{
                    AppendMenuW, CreatePopupMenu, CreateWindowExW, CREATESTRUCTW,
                    DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos,
                    GetMessageW, GetWindowLongPtrW, HMENU, IDI_APPLICATION, LoadIconW,
                    MF_DISABLED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW,
                    PostQuitMessage, RegisterClassW, SetForegroundWindow, SetWindowLongPtrW,
                    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu,
                    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_COMMAND,
                    WM_DESTROY, WM_NCCREATE, WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
                    GWLP_USERDATA,
                },
            },
        },
    };

    use super::{TrayError, TrayHandle};

    const WM_TRAYICON: u32 = WM_APP + 1;
    const MENU_EXIT_ID: usize = 1001;

    pub struct TrayHandleImpl {
        shutdown_tx: Option<Sender<()>>,
        thread: Option<JoinHandle<()>>,
    }

    impl std::fmt::Debug for TrayHandleImpl {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("TrayHandleImpl").finish_non_exhaustive()
        }
    }

    impl TrayHandleImpl {
        pub fn shutdown(&mut self) {
            if let Some(shutdown_tx) = self.shutdown_tx.take() {
                let _ = shutdown_tx.send(());
            }

            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    struct WindowState {
        shared_state: Arc<SharedAppState>,
        nid: NOTIFYICONDATAW,
    }

    pub fn run(state: Arc<SharedAppState>) -> Result<TrayHandle, TrayError> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (shutdown_tx, shutdown_rx) = mpsc::channel();

        let thread = thread::Builder::new()
            .name("ruler-app-tray".to_string())
            .spawn(move || tray_thread_entry(state, shutdown_rx, ready_tx))
            .map_err(|error| TrayError::new(format!("failed to start tray thread: {error}")))?;

        ready_rx
            .recv()
            .map_err(|error| TrayError::new(format!("failed to receive tray startup status: {error}")))?
            .map_err(TrayError::new)?;

        Ok(TrayHandle::new(TrayHandleImpl {
            shutdown_tx: Some(shutdown_tx),
            thread: Some(thread),
        }))
    }

    fn tray_thread_entry(
        state: Arc<SharedAppState>,
        shutdown_rx: Receiver<()>,
        ready_tx: Sender<Result<(), String>>,
    ) {
        let result = unsafe { create_tray_window(state) };

        match result {
            Ok(hwnd) => {
                let _ = ready_tx.send(Ok(()));
                run_message_loop(hwnd, shutdown_rx);
            }
            Err(error) => {
                let _ = ready_tx.send(Err(error.to_string()));
            }
        }
    }

    fn run_message_loop(hwnd: HWND, shutdown_rx: Receiver<()>) {
        loop {
            if shutdown_rx.try_recv().is_ok() {
                unsafe {
                    let _ = PostMessageW(hwnd, WM_DESTROY, WPARAM(0), LPARAM(0));
                }
            }

            let mut message = MSG::default();
            let status = unsafe { GetMessageW(&mut message, HWND::default(), 0, 0) }.0;
            if status <= 0 {
                break;
            }

            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    unsafe fn create_tray_window(shared_state: Arc<SharedAppState>) -> Result<HWND, TrayError> {
        let instance = GetModuleHandleW(PCWSTR::null())
            .map_err(|error| TrayError::new(format!("GetModuleHandleW failed: {error}")))?;

        let class_name = wide("RulerTrayWindowClass");
        let icon = LoadIconW(HINSTANCE::default(), IDI_APPLICATION)
            .map_err(|error| TrayError::new(format!("LoadIconW failed: {error}")))?;

        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: HINSTANCE(instance.0),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            hbrBackground: HBRUSH::default(),
            ..Default::default()
        };

        if RegisterClassW(&class) == 0 {
            return Err(TrayError::new("RegisterClassW failed for tray window"));
        }

        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAYICON;
        nid.hIcon = icon;
        nid.szTip = tooltip_text(&shared_state.snapshot().status_text);

        let state = Box::new(WindowState { shared_state, nid });
        let state_ptr = Box::into_raw(state);

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(class_name.as_ptr()),
            WINDOW_STYLE(WS_OVERLAPPED.0),
            0,
            0,
            0,
            0,
            HWND::default(),
            HMENU::default(),
            HINSTANCE(instance.0),
            Some(state_ptr.cast()),
        )
        .map_err(|error| TrayError::new(format!("CreateWindowExW failed: {error}")))?;

        if hwnd.0.is_null() {
            let _ = Box::from_raw(state_ptr);
            return Err(TrayError::new("CreateWindowExW returned a null tray window"));
        }

        Ok(hwnd)
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

                let state = &mut *state_ptr;
                state.nid.hWnd = hwnd;
                if !Shell_NotifyIconW(NIM_ADD, &state.nid).as_bool() {
                    let _ = Box::from_raw(state_ptr);
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    return LRESULT(0);
                }

                LRESULT(1)
            }
            WM_COMMAND => {
                if (wparam.0 & 0xffff) == MENU_EXIT_ID {
                    let _ = DestroyWindow(hwnd);
                    ExitProcess(0);
                }
                LRESULT(0)
            }
            WM_TRAYICON => {
                if lparam.0 as u32 == WM_RBUTTONUP {
                    show_tray_menu(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
                if !state_ptr.is_null() {
                    let state = Box::from_raw(state_ptr);
                    let _ = Shell_NotifyIconW(NIM_DELETE, &state.nid);
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn show_tray_menu(hwnd: HWND) {
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
        if state_ptr.is_null() {
            return;
        }

        let state = &mut *state_ptr;
        let snapshot = state.shared_state.snapshot();
        state.nid.szTip = tooltip_text(&snapshot.status_text);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &state.nid);

        let Ok(menu) = CreatePopupMenu() else {
            return;
        };

        let status = wide(&truncate_menu_text(&snapshot.status_text));
        let frame = wide(&truncate_menu_text(&snapshot.frame_text));
        let timer = wide(&truncate_menu_text(&snapshot.timer_text));
        let exit = wide("Exit");

        let disabled = MF_STRING | MF_DISABLED | MF_GRAYED;
        let _ = AppendMenuW(menu, disabled, 1, PCWSTR(status.as_ptr()));
        let _ = AppendMenuW(menu, disabled, 2, PCWSTR(frame.as_ptr()));
        let _ = AppendMenuW(menu, disabled, 3, PCWSTR(timer.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT_ID, PCWSTR(exit.as_ptr()));

        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
            cursor.x,
            cursor.y,
            0,
            hwnd,
            None,
        );
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }

    fn truncate_menu_text(value: &str) -> String {
        const LIMIT: usize = 64;
        if value.chars().count() <= LIMIT {
            value.to_string()
        } else {
            let shortened: String = value.chars().take(LIMIT - 1).collect();
            format!("{shortened}…")
        }
    }

    fn tooltip_text(status: &str) -> [u16; 128] {
        let text = truncate_menu_text(status);
        let mut buf = [0u16; 128];
        for (index, value) in text.encode_utf16().take(buf.len() - 1).enumerate() {
            buf[index] = value;
        }
        buf
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(iter::once(0)).collect()
    }
}
