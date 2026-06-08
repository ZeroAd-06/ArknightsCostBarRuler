use std::{
    ffi::c_void,
    iter,
    sync::{mpsc::Sender, Arc},
};

use crate::{
    commands::UiCommand,
    i18n::I18n,
    ui_state::{FrameDisplayMode, FRAMES_PER_SECOND, VERSION},
    worker::SharedAppState,
};

#[cfg(windows)]
pub mod win32 {
    use super::*;
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                Shell::ShellExecuteW,
                WindowsAndMessaging::{
                    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics,
                    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, MessageBoxW,
                    PostMessageW, RegisterClassW, SetForegroundWindow, SetWindowLongPtrW,
                    SetWindowPos, ShowWindow, TrackPopupMenu, TranslateMessage, BS_DEFPUSHBUTTON,
                    BS_PUSHBUTTON, ES_AUTOHSCROLL, GWLP_USERDATA, HMENU, IDCANCEL, IDOK, IDYES,
                    MB_ICONWARNING, MB_OK, MB_YESNO, MF_CHECKED, MF_DISABLED, MF_GRAYED, MF_POPUP,
                    MF_SEPARATOR, MF_STRING, MSG, SM_CXSCREEN, SM_CYSCREEN, SWP_NOSIZE,
                    SWP_NOZORDER, SW_SHOW, SW_SHOWNORMAL, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
                    TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND,
                    WM_CREATE, WM_DESTROY, WM_NCCREATE, WM_NULL, WNDCLASSW, WS_BORDER, WS_CAPTION,
                    WS_CHILD, WS_EX_CLIENTEDGE, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
                },
            },
        },
    };

    const ID_NEW_PROFILE: usize = 2000;
    const ID_DISPLAY_ZERO_TO_N_MINUS_ONE: usize = 2100;
    const ID_DISPLAY_ZERO_TO_N: usize = 2101;
    const ID_DISPLAY_ONE_TO_N: usize = 2102;
    const ID_TIMER_BACK_CYCLE: usize = 2200;
    const ID_TIMER_BACK_SECOND: usize = 2201;
    const ID_TIMER_RESET: usize = 2202;
    const ID_TIMER_FORWARD_SECOND: usize = 2203;
    const ID_TIMER_FORWARD_CYCLE: usize = 2204;
    const ID_ABOUT: usize = 2300;
    const ID_EXIT: usize = 2301;
    const ID_PROFILE_SELECT_BASE: usize = 3000;
    const ID_PROFILE_RENAME_BASE: usize = 4000;
    const ID_PROFILE_DELETE_BASE: usize = 5000;

    pub unsafe fn show_context_menu(hwnd: HWND, state: &SharedAppState, i18n: &I18n) {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        append_root_menu(menu, state, i18n);

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

    pub unsafe fn handle_menu_command(
        hwnd: HWND,
        command_id: usize,
        state: &Arc<SharedAppState>,
        command_tx: &Sender<UiCommand>,
        i18n: &I18n,
    ) {
        match command_id {
            ID_NEW_PROFILE => send(command_tx, UiCommand::PrepareCalibration),
            ID_DISPLAY_ZERO_TO_N_MINUS_ONE => send(
                command_tx,
                UiCommand::SetDisplayMode(FrameDisplayMode::ZeroToNMinusOne),
            ),
            ID_DISPLAY_ZERO_TO_N => send(
                command_tx,
                UiCommand::SetDisplayMode(FrameDisplayMode::ZeroToN),
            ),
            ID_DISPLAY_ONE_TO_N => send(
                command_tx,
                UiCommand::SetDisplayMode(FrameDisplayMode::OneToN),
            ),
            ID_TIMER_BACK_CYCLE => adjust_cycle(command_tx, state, -1),
            ID_TIMER_BACK_SECOND => send(
                command_tx,
                UiCommand::AdjustTimer {
                    frames: -FRAMES_PER_SECOND,
                },
            ),
            ID_TIMER_RESET => send(command_tx, UiCommand::ResetTimer),
            ID_TIMER_FORWARD_SECOND => send(
                command_tx,
                UiCommand::AdjustTimer {
                    frames: FRAMES_PER_SECOND,
                },
            ),
            ID_TIMER_FORWARD_CYCLE => adjust_cycle(command_tx, state, 1),
            ID_ABOUT => open_about_page(),
            ID_EXIT => send(command_tx, UiCommand::Exit),
            id if (ID_PROFILE_SELECT_BASE..ID_PROFILE_SELECT_BASE + 500).contains(&id) => {
                if let Some(profile) = state
                    .snapshot()
                    .ui
                    .profiles
                    .get(id - ID_PROFILE_SELECT_BASE)
                {
                    send(
                        command_tx,
                        UiCommand::UseProfile {
                            filename: profile.filename.clone(),
                        },
                    );
                }
            }
            id if (ID_PROFILE_RENAME_BASE..ID_PROFILE_RENAME_BASE + 500).contains(&id) => {
                if let Some(profile) = state
                    .snapshot()
                    .ui
                    .profiles
                    .get(id - ID_PROFILE_RENAME_BASE)
                    .cloned()
                {
                    let prompt = i18n.tr_with(
                        "overlay.dialog.rename.prompt",
                        &[("old_basename", profile.basename.clone())],
                    );
                    if let Some(new_base) = prompt_text(
                        hwnd,
                        &i18n.tr("overlay.dialog.rename.title"),
                        &prompt,
                        &profile.basename,
                    ) {
                        if new_base.trim().is_empty() {
                            show_message(
                                hwnd,
                                &i18n.tr("overlay.error.name_empty.title"),
                                &i18n.tr("overlay.error.name_empty"),
                            );
                        } else {
                            send(
                                command_tx,
                                UiCommand::RenameProfile {
                                    old: profile.filename,
                                    new_base,
                                },
                            );
                        }
                    }
                }
            }
            id if (ID_PROFILE_DELETE_BASE..ID_PROFILE_DELETE_BASE + 500).contains(&id) => {
                if let Some(profile) = state
                    .snapshot()
                    .ui
                    .profiles
                    .get(id - ID_PROFILE_DELETE_BASE)
                {
                    let message = i18n.tr_with(
                        "overlay.dialog.delete.msg",
                        &[("basename", profile.basename.clone())],
                    );
                    if confirm(hwnd, &i18n.tr("overlay.dialog.delete.title"), &message) {
                        send(
                            command_tx,
                            UiCommand::DeleteProfile {
                                filename: profile.filename.clone(),
                            },
                        );
                    }
                }
            }
            _ => {}
        }
    }

    unsafe fn append_root_menu(menu: HMENU, state: &SharedAppState, i18n: &I18n) {
        let snapshot = state.snapshot();
        let calibration = CreatePopupMenu().ok();
        let display = CreatePopupMenu().ok();
        let timer = CreatePopupMenu().ok();

        if let Some(calibration) = calibration {
            append_profile_menu(calibration, &snapshot.ui, i18n);
            append_cascade(menu, &i18n.tr("overlay.menu.calibration"), calibration);
        }
        if let Some(display) = display {
            append_display_menu(display, snapshot.ui.display_mode);
            append_cascade(menu, &i18n.tr("overlay.menu.display"), display);
        }
        if let Some(timer) = timer {
            append_timer_menu(timer, &snapshot.ui, i18n);
            append_cascade(menu, &i18n.tr("overlay.menu.timer"), timer);
        }

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        append_string(
            menu,
            ID_ABOUT,
            &i18n.tr_with("overlay.menu.about", &[("version", VERSION.to_string())]),
            true,
            false,
        );
        append_string(menu, ID_EXIT, &i18n.tr("overlay.menu.exit"), true, false);
    }

    unsafe fn append_profile_menu(menu: HMENU, ui: &crate::ui_state::UiSnapshot, i18n: &I18n) {
        append_string(
            menu,
            ID_NEW_PROFILE,
            &i18n.tr("overlay.menu.new_profile"),
            true,
            false,
        );
        if !ui.profiles.is_empty() {
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        }
        for (index, profile) in ui.profiles.iter().enumerate() {
            let Ok(actions) = CreatePopupMenu() else {
                continue;
            };
            append_string(
                actions,
                ID_PROFILE_SELECT_BASE + index,
                &i18n.tr("overlay.menu.select"),
                !profile.is_active,
                false,
            );
            append_string(
                actions,
                ID_PROFILE_RENAME_BASE + index,
                &i18n.tr("overlay.menu.rename"),
                true,
                false,
            );
            append_string(
                actions,
                ID_PROFILE_DELETE_BASE + index,
                &i18n.tr("overlay.menu.delete"),
                true,
                false,
            );
            let prefix = if profile.is_active { "● " } else { "" };
            append_cascade(
                menu,
                &format!(
                    "{prefix}{} ({})",
                    profile.basename, profile.total_frames_str
                ),
                actions,
            );
        }
    }

    unsafe fn append_display_menu(menu: HMENU, current: FrameDisplayMode) {
        for (mode, id) in [
            (
                FrameDisplayMode::ZeroToNMinusOne,
                ID_DISPLAY_ZERO_TO_N_MINUS_ONE,
            ),
            (FrameDisplayMode::ZeroToN, ID_DISPLAY_ZERO_TO_N),
            (FrameDisplayMode::OneToN, ID_DISPLAY_ONE_TO_N),
        ] {
            append_string(menu, id, mode.label(), true, current == mode);
        }
    }

    unsafe fn append_timer_menu(menu: HMENU, ui: &crate::ui_state::UiSnapshot, i18n: &I18n) {
        let enabled = ui.active_profile.is_some();
        let cycle_frames = ui.total_frames_in_cycle;
        append_string(
            menu,
            ID_TIMER_BACK_CYCLE,
            &i18n.tr_with(
                "overlay.timer.back_frames",
                &[("frames", cycle_frames.to_string())],
            ),
            enabled,
            false,
        );
        append_string(
            menu,
            ID_TIMER_BACK_SECOND,
            &i18n.tr("overlay.timer.back_1s"),
            enabled,
            false,
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        append_string(
            menu,
            ID_TIMER_RESET,
            &i18n.tr("overlay.timer.reset"),
            enabled,
            false,
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        append_string(
            menu,
            ID_TIMER_FORWARD_SECOND,
            &i18n.tr("overlay.timer.fwd_1s"),
            enabled,
            false,
        );
        append_string(
            menu,
            ID_TIMER_FORWARD_CYCLE,
            &i18n.tr_with(
                "overlay.timer.fwd_frames",
                &[("frames", cycle_frames.to_string())],
            ),
            enabled,
            false,
        );
    }

    unsafe fn append_cascade(menu: HMENU, text: &str, submenu: HMENU) {
        let text = wide(text);
        let _ = AppendMenuW(menu, MF_POPUP, submenu.0 as usize, PCWSTR(text.as_ptr()));
    }

    unsafe fn append_string(menu: HMENU, id: usize, text: &str, enabled: bool, checked: bool) {
        let mut flags = MF_STRING;
        if !enabled {
            flags |= MF_DISABLED | MF_GRAYED;
        }
        if checked {
            flags |= MF_CHECKED;
        }
        let text = wide(text);
        let _ = AppendMenuW(menu, flags, id, PCWSTR(text.as_ptr()));
    }

    fn send(command_tx: &Sender<UiCommand>, command: UiCommand) {
        let _ = command_tx.send(command);
    }

    fn adjust_cycle(command_tx: &Sender<UiCommand>, state: &SharedAppState, direction: i32) {
        let frames = state.snapshot().ui.total_frames_in_cycle * direction;
        send(command_tx, UiCommand::AdjustTimer { frames });
    }

    unsafe fn open_about_page() {
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

    unsafe fn prompt_text(owner: HWND, title: &str, prompt: &str, initial: &str) -> Option<String> {
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
                let create_struct =
                    lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
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

    unsafe fn confirm(hwnd: HWND, title: &str, message: &str) -> bool {
        let title = wide(title);
        let message = wide(message);
        MessageBoxW(
            hwnd,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_YESNO | MB_ICONWARNING,
        ) == IDYES
    }

    unsafe fn show_message(hwnd: HWND, title: &str, message: &str) {
        let title = wide(title);
        let message = wide(message);
        let _ = MessageBoxW(
            hwnd,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK,
        );
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(iter::once(0)).collect()
    }
}
