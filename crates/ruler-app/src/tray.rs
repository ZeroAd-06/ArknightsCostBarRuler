use std::{
    fmt,
    sync::{mpsc::Sender, Arc},
};

use crate::{commands::UiCommand, i18n::I18n, icons::IconSet, worker::SharedAppState};

#[derive(Debug)]
pub struct TrayRuntime {
    state: Arc<SharedAppState>,
    command_tx: Sender<UiCommand>,
    i18n: Arc<I18n>,
    icons: Arc<IconSet>,
}

impl TrayRuntime {
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
        format!("native tray runtime ready with mode={:?}", snapshot.ui.mode)
    }

    pub fn run(&self) -> Result<TrayHandle, TrayError> {
        platform::run(
            Arc::clone(&self.state),
            self.command_tx.clone(),
            Arc::clone(&self.i18n),
            Arc::clone(&self.icons),
        )
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
    use std::sync::{mpsc::Sender, Arc};

    use crate::{commands::UiCommand, i18n::I18n, icons::IconSet, worker::SharedAppState};

    use super::{TrayError, TrayHandle};

    #[derive(Debug)]
    pub struct TrayHandleImpl;

    impl TrayHandleImpl {
        pub fn shutdown(&mut self) {}
    }

    pub fn run(
        _: Arc<SharedAppState>,
        _: Sender<UiCommand>,
        _: Arc<I18n>,
        _: Arc<IconSet>,
    ) -> Result<TrayHandle, TrayError> {
        Ok(TrayHandle::new(TrayHandleImpl))
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        iter, mem,
        sync::{
            atomic::{AtomicIsize, Ordering},
            mpsc::{self, Sender},
            Arc,
        },
        thread::{self, JoinHandle},
    };

    use crate::{
        commands::UiCommand,
        i18n::I18n,
        icons::{win32::create_icon, IconSet},
        menu,
        worker::SharedAppState,
    };
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
            Graphics::Gdi::HBRUSH,
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                Shell::{
                    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
                    NIM_MODIFY, NOTIFYICONDATAW,
                },
                WindowsAndMessaging::{
                    CreateWindowExW, DefWindowProcW, DestroyIcon, DispatchMessageW, GetMessageW,
                    GetWindowLongPtrW, LoadIconW, PostMessageW, PostQuitMessage, RegisterClassW,
                    SetWindowLongPtrW, TranslateMessage, CREATESTRUCTW, GWLP_USERDATA, HICON,
                    HMENU, IDI_APPLICATION, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_COMMAND,
                    WM_DESTROY, WM_NCCREATE, WM_RBUTTONUP, WNDCLASSW, WS_EX_TOOLWINDOW,
                    WS_OVERLAPPED,
                },
            },
        },
    };

    use super::{TrayError, TrayHandle};

    const WM_TRAYICON: u32 = WM_APP + 1;

    pub struct TrayHandleImpl {
        hwnd: Arc<AtomicIsize>,
        thread: Option<JoinHandle<()>>,
    }

    impl std::fmt::Debug for TrayHandleImpl {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("TrayHandleImpl").finish_non_exhaustive()
        }
    }

    impl TrayHandleImpl {
        pub fn shutdown(&mut self) {
            let raw_hwnd = self.hwnd.load(Ordering::Relaxed);
            if raw_hwnd != 0 {
                unsafe {
                    let _ =
                        PostMessageW(HWND(raw_hwnd as *mut _), WM_DESTROY, WPARAM(0), LPARAM(0));
                }
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    struct WindowState {
        shared_state: Arc<SharedAppState>,
        command_tx: Sender<UiCommand>,
        i18n: Arc<I18n>,
        nid: NOTIFYICONDATAW,
        custom_icon: Option<HICON>,
    }

    pub fn run(
        state: Arc<SharedAppState>,
        command_tx: Sender<UiCommand>,
        i18n: Arc<I18n>,
        icons: Arc<IconSet>,
    ) -> Result<TrayHandle, TrayError> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let hwnd = Arc::new(AtomicIsize::new(0));
        let hwnd_for_thread = Arc::clone(&hwnd);

        let thread = thread::Builder::new()
            .name("ruler-app-tray".to_string())
            .spawn(move || {
                tray_thread_entry(state, command_tx, i18n, icons, hwnd_for_thread, ready_tx)
            })
            .map_err(|error| TrayError::new(format!("failed to start tray thread: {error}")))?;

        ready_rx
            .recv()
            .map_err(|error| {
                TrayError::new(format!("failed to receive tray startup status: {error}"))
            })?
            .map_err(TrayError::new)?;

        Ok(TrayHandle::new(TrayHandleImpl {
            hwnd,
            thread: Some(thread),
        }))
    }

    fn tray_thread_entry(
        state: Arc<SharedAppState>,
        command_tx: Sender<UiCommand>,
        i18n: Arc<I18n>,
        icons: Arc<IconSet>,
        hwnd_holder: Arc<AtomicIsize>,
        ready_tx: Sender<Result<(), String>>,
    ) {
        let result = unsafe { create_tray_window(state, command_tx, i18n, icons) };
        match result {
            Ok(hwnd) => {
                hwnd_holder.store(hwnd.0 as isize, Ordering::Relaxed);
                let _ = ready_tx.send(Ok(()));
                run_message_loop();
                hwnd_holder.store(0, Ordering::Relaxed);
            }
            Err(error) => {
                let _ = ready_tx.send(Err(error.to_string()));
            }
        }
    }

    fn run_message_loop() {
        let mut message = MSG::default();
        loop {
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

    unsafe fn create_tray_window(
        shared_state: Arc<SharedAppState>,
        command_tx: Sender<UiCommand>,
        i18n: Arc<I18n>,
        icons: Arc<IconSet>,
    ) -> Result<HWND, TrayError> {
        let instance = GetModuleHandleW(PCWSTR::null())
            .map_err(|error| TrayError::new(format!("GetModuleHandleW failed: {error}")))?;

        let class_name = wide("RulerTrayWindowClass");
        let custom_icon = icons.get("deco").and_then(|icon| create_icon(icon, 32));
        let icon = if let Some(icon) = custom_icon {
            icon
        } else {
            LoadIconW(HINSTANCE::default(), IDI_APPLICATION)
                .map_err(|error| TrayError::new(format!("LoadIconW failed: {error}")))?
        };

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
        nid.szTip = tooltip_text("明日方舟费用条尺子");

        let state = Box::new(WindowState {
            shared_state,
            command_tx,
            i18n,
            nid,
            custom_icon,
        });
        let state_ptr = Box::into_raw(state);

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0),
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
            return Err(TrayError::new(
                "CreateWindowExW returned a null tray window",
            ));
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
                if let Some(state) = window_state(hwnd) {
                    menu::win32::handle_menu_command(
                        hwnd,
                        wparam.0 & 0xffff,
                        &state.shared_state,
                        &state.command_tx,
                        &state.i18n,
                    );
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
                    if let Some(icon) = state.custom_icon {
                        let _ = DestroyIcon(icon);
                    }
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn show_tray_menu(hwnd: HWND) {
        let Some(state) = window_state(hwnd) else {
            return;
        };
        let snapshot = state.shared_state.snapshot();
        let mut nid = state.nid;
        nid.szTip = tooltip_text(match snapshot.ui.mode {
            crate::ui_state::OverlayMode::Running => "明日方舟费用条尺子",
            _ => &snapshot.ui.message,
        });
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        menu::win32::show_context_menu(hwnd, &state.shared_state, &state.i18n);
    }

    unsafe fn window_state<'a>(hwnd: HWND) -> Option<&'a WindowState> {
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
        if state_ptr.is_null() {
            None
        } else {
            Some(&*state_ptr)
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

    fn truncate_menu_text(value: &str) -> String {
        const LIMIT: usize = 64;
        if value.chars().count() <= LIMIT {
            value.to_string()
        } else {
            let shortened: String = value.chars().take(LIMIT - 1).collect();
            format!("{shortened}…")
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(iter::once(0)).collect()
    }
}
