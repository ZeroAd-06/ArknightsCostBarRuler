//! Native modal dialogs used by the overlay: the rename text-input prompt, the
//! delete confirmation, an error message box, and the "about" page launcher.
//!
//! The right-click menu itself is now a Slint popup (see `overlay.rs`); only
//! these blocking dialogs remain native for the moment.

#[cfg(windows)]
pub mod win32 {
    use std::{ffi::c_void, iter};

    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                Shell::ShellExecuteW,
                WindowsAndMessaging::{
                    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
                    GetSystemMetrics, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW,
                    MessageBoxW, RegisterClassW, SetWindowLongPtrW, SetWindowPos, ShowWindow,
                    TranslateMessage, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CREATESTRUCTW,
                    ES_AUTOHSCROLL, GWLP_USERDATA, HMENU, IDCANCEL, IDOK, IDYES, MB_ICONWARNING,
                    MB_OK, MB_YESNO, MSG, SM_CXSCREEN, SM_CYSCREEN, SWP_NOSIZE, SWP_NOZORDER,
                    SW_SHOW, SW_SHOWNORMAL, WINDOW_EX_STYLE, WINDOW_STYLE, WS_BORDER, WS_CAPTION,
                    WS_CHILD, WS_EX_CLIENTEDGE, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
                    WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_NCCREATE, WNDCLASSW,
                },
            },
        },
    };

    pub unsafe fn open_about_page() {
        let operation = wide("open");
        let url = wide("https://github.com/ZeroAd-06/ArknightsCostBarRuler");
        let _ = ShellExecuteW(
            HWND::default(),
            PCWSTR(operation.as_ptr()),
            PCWSTR(url.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }

    struct InputDialogState {
        prompt: String,
        initial: String,
        edit: HWND,
        result: Option<String>,
        done: bool,
    }

    pub unsafe fn prompt_text(owner: HWND, title: &str, prompt: &str, initial: &str) -> Option<String> {
        const CLASS_NAME: &str = "RulerProfileRenameDialog";
        let Ok(module) = GetModuleHandleW(PCWSTR::null()) else {
            return None;
        };
        let class_name = wide(CLASS_NAME);
        let class = WNDCLASSW {
            lpfnWndProc: Some(input_dialog_proc),
            hInstance: HINSTANCE(module.0),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);

        let mut state = Box::new(InputDialogState {
            prompt: prompt.to_string(),
            initial: initial.to_string(),
            edit: HWND::default(),
            result: None,
            done: false,
        });
        let state_ptr = state.as_mut() as *mut InputDialogState;
        let title = wide(title);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0),
            0,
            0,
            380,
            170,
            owner,
            HMENU::default(),
            HINSTANCE(module.0),
            Some(state_ptr.cast()),
        )
        .ok()?;
        if hwnd.0.is_null() {
            return None;
        }

        center_window(hwnd, 380, 170);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let mut message = MSG::default();
        while !state.done && GetMessageW(&mut message, HWND::default(), 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        state.result.take()
    }

    unsafe extern "system" fn input_dialog_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_NCCREATE => {
                let create_struct = lparam.0 as *const CREATESTRUCTW;
                let state_ptr = (*create_struct).lpCreateParams as *mut InputDialogState;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
                LRESULT(1)
            }
            WM_CREATE => {
                if let Some(state) = input_state_mut(hwnd) {
                    create_dialog_static(hwnd, 16, 18, 340, 24, &state.prompt);
                    state.edit = create_dialog_control(
                        hwnd,
                        "EDIT",
                        &state.initial,
                        100,
                        16,
                        50,
                        340,
                        24,
                        WINDOW_STYLE(
                            WS_CHILD.0
                                | WS_VISIBLE.0
                                | WS_TABSTOP.0
                                | WS_BORDER.0
                                | ES_AUTOHSCROLL as u32,
                        ),
                        WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0),
                    );
                    create_dialog_control(
                        hwnd,
                        "BUTTON",
                        "OK",
                        IDOK.0,
                        190,
                        92,
                        80,
                        28,
                        WINDOW_STYLE(
                            WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
                        ),
                        WINDOW_EX_STYLE::default(),
                    );
                    create_dialog_control(
                        hwnd,
                        "BUTTON",
                        "Cancel",
                        IDCANCEL.0,
                        280,
                        92,
                        80,
                        28,
                        WINDOW_STYLE(
                            WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
                        ),
                        WINDOW_EX_STYLE::default(),
                    );
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                if let Some(state) = input_state_mut(hwnd) {
                    let control_id = (wparam.0 & 0xffff) as i32;
                    if control_id == IDOK.0 {
                        state.result = Some(edit_text(state.edit).trim().to_string());
                        state.done = true;
                        let _ = DestroyWindow(hwnd);
                    } else if control_id == IDCANCEL.0 {
                        state.result = None;
                        state.done = true;
                        let _ = DestroyWindow(hwnd);
                    }
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                if let Some(state) = input_state_mut(hwnd) {
                    state.result = None;
                    state.done = true;
                }
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                if let Some(state) = input_state_mut(hwnd) {
                    state.done = true;
                }
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn create_dialog_static(
        hwnd: HWND,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        text: &str,
    ) -> HWND {
        create_dialog_control(
            hwnd,
            "STATIC",
            text,
            0,
            x,
            y,
            width,
            height,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
            WINDOW_EX_STYLE::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn create_dialog_control(
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

    unsafe fn edit_text(hwnd: HWND) -> String {
        let len = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; len.max(0) as usize + 1];
        let count = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..count as usize])
    }

    unsafe fn input_state_mut<'a>(hwnd: HWND) -> Option<&'a mut InputDialogState> {
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut InputDialogState;
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

    pub unsafe fn confirm(hwnd: HWND, title: &str, message: &str) -> bool {
        let title = wide(title);
        let message = wide(message);
        MessageBoxW(
            hwnd,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_YESNO | MB_ICONWARNING,
        ) == IDYES
    }

    pub unsafe fn show_message(hwnd: HWND, title: &str, message: &str) {
        let title = wide(title);
        let message = wide(message);
        let _ = MessageBoxW(hwnd, PCWSTR(message.as_ptr()), PCWSTR(title.as_ptr()), MB_OK);
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(iter::once(0)).collect()
    }
}
