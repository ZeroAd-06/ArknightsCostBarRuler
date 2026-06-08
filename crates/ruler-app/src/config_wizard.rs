use ruler_core::RulerConfig;

use crate::{i18n::I18n, resources::ResourceLocator};

pub fn run_config_wizard(_resources: &ResourceLocator, i18n: &I18n) -> Option<RulerConfig> {
    platform::run_config_wizard(i18n)
}

#[cfg(not(windows))]
mod platform {
    use ruler_core::RulerConfig;

    use crate::i18n::I18n;

    pub fn run_config_wizard(_: &I18n) -> Option<RulerConfig> {
        None
    }
}

#[cfg(windows)]
mod platform {
    use std::{ffi::c_void, iter};

    use ruler_core::RulerConfig;
    use windows::{
        core::{PCWSTR, PWSTR},
        Win32::{
            Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
            System::{Com::CoTaskMemFree, LibraryLoader::GetModuleHandleW},
            UI::{
                Shell::{
                    SHBrowseForFolderW, SHGetPathFromIDListW, BIF_NEWDIALOGSTYLE,
                    BIF_RETURNONLYFSDIRS, BROWSEINFOW,
                },
                WindowsAndMessaging::{
                    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows,
                    GetClassNameW, GetMessageW, GetSystemMetrics, GetWindowLongPtrW,
                    GetWindowTextLengthW, GetWindowTextW, IsWindowVisible, LoadIconW, MessageBoxW,
                    RegisterClassW, SendMessageW, SetWindowLongPtrW, SetWindowPos, SetWindowTextW,
                    ShowWindow, TranslateMessage, BS_DEFPUSHBUTTON, BS_GROUPBOX, BS_PUSHBUTTON,
                    CBN_SELCHANGE, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL,
                    ES_AUTOHSCROLL, GWLP_USERDATA, HMENU, IDCANCEL, IDI_APPLICATION, LBN_SELCHANGE,
                    LB_ADDSTRING, LB_GETCURSEL, LB_RESETCONTENT, LB_SETCURSEL, MB_ICONERROR, MB_OK,
                    MSG, SM_CXSCREEN, SM_CYSCREEN, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOW,
                    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY,
                    WM_NCCREATE, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE,
                    WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
                },
            },
        },
    };

    use crate::i18n::I18n;

    const CLASS_NAME: &str = "RulerConfigWizardWindow";
    const ID_LANGUAGE: i32 = 1000;
    const ID_CAPTURE_TYPE: i32 = 1001;
    const ID_PATH_LABEL: i32 = 1002;
    const ID_PATH_EDIT: i32 = 1003;
    const ID_BROWSE: i32 = 1004;
    const ID_INSTANCE_LABEL: i32 = 1005;
    const ID_INSTANCE_EDIT: i32 = 1006;
    const ID_DEVICE_LABEL: i32 = 1007;
    const ID_DEVICE_EDIT: i32 = 1008;
    const ID_SCAN: i32 = 1009;
    const ID_WINDOW_LIST: i32 = 1010;
    const ID_SELECTED_LABEL: i32 = 1011;
    const ID_SAVE: i32 = 1012;
    const ID_DYNAMIC_GROUP: i32 = 1013;

    #[derive(Clone, Debug)]
    struct WindowCandidate {
        hwnd: isize,
        title: String,
        class_name: String,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum CaptureChoice {
        Mumu,
        LdPlayer,
        Minicap,
        Window,
    }

    struct WizardState {
        i18n: I18n,
        hwnd: HWND,
        result: Option<RulerConfig>,
        done: bool,
        dynamic_group: HWND,
        language_combo: HWND,
        capture_combo: HWND,
        path_label: HWND,
        path_edit: HWND,
        browse_button: HWND,
        instance_label: HWND,
        instance_edit: HWND,
        device_label: HWND,
        device_edit: HWND,
        scan_button: HWND,
        window_list: HWND,
        selected_label: HWND,
        save_button: HWND,
        candidates: Vec<WindowCandidate>,
        selected_window: Option<usize>,
    }

    impl WizardState {
        fn new(i18n: &I18n) -> Self {
            Self {
                i18n: i18n.clone(),
                hwnd: HWND::default(),
                result: None,
                done: false,
                dynamic_group: HWND::default(),
                language_combo: HWND::default(),
                capture_combo: HWND::default(),
                path_label: HWND::default(),
                path_edit: HWND::default(),
                browse_button: HWND::default(),
                instance_label: HWND::default(),
                instance_edit: HWND::default(),
                device_label: HWND::default(),
                device_edit: HWND::default(),
                scan_button: HWND::default(),
                window_list: HWND::default(),
                selected_label: HWND::default(),
                save_button: HWND::default(),
                candidates: Vec::new(),
                selected_window: None,
            }
        }
    }

    pub fn run_config_wizard(i18n: &I18n) -> Option<RulerConfig> {
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

            let mut state = Box::new(WizardState::new(i18n));
            let state_ptr = state.as_mut() as *mut WizardState;
            let title = wide(&i18n.tr("config.window.title"));
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0),
                0,
                0,
                620,
                420,
                HWND::default(),
                HMENU::default(),
                HINSTANCE(module.0),
                Some(state_ptr.cast()),
            )
            .ok()?;
            if hwnd.0.is_null() {
                return None;
            }

            center_window(hwnd, 620, 420);
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
                    update_dynamic_controls(state);
                    scan_windows_into_state(state);
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                if let Some(state) = state_mut(hwnd) {
                    let control_id = (wparam.0 & 0xffff) as i32;
                    let notification = ((wparam.0 >> 16) & 0xffff) as u32;
                    match control_id {
                        ID_CAPTURE_TYPE if notification == CBN_SELCHANGE => {
                            update_dynamic_controls(state);
                        }
                        ID_SCAN => scan_windows_into_state(state),
                        ID_WINDOW_LIST if notification == LBN_SELCHANGE => {
                            select_window_from_list(state)
                        }
                        ID_BROWSE => browse_install_path(state),
                        ID_SAVE => save_config(state),
                        id if id == IDCANCEL.0 => close_without_result(state),
                        _ => {}
                    }
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
                if let Some(state) = state_mut(hwnd) {
                    state.done = true;
                }
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn create_controls(hwnd: HWND, state: &mut WizardState) {
        create_static(hwnd, 20, 18, 560, 24, &state.i18n.tr("config.header"));
        create_static(hwnd, 20, 54, 150, 22, &state.i18n.tr("config.language"));
        state.language_combo = create_control(
            hwnd,
            "COMBOBOX",
            "",
            ID_LANGUAGE,
            180,
            50,
            180,
            120,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | CBS_DROPDOWNLIST as u32),
            WINDOW_EX_STYLE::default(),
        );
        add_combo_item(state.language_combo, "zh_CN");
        add_combo_item(state.language_combo, "en_US");
        let language_index = if state.i18n.locale() == "en_US" { 1 } else { 0 };
        let _ = SendMessageW(
            state.language_combo,
            CB_SETCURSEL,
            WPARAM(language_index),
            LPARAM(0),
        );

        create_static(
            hwnd,
            20,
            88,
            150,
            22,
            &state.i18n.tr("config.emulator_type"),
        );
        state.capture_combo = create_control(
            hwnd,
            "COMBOBOX",
            "",
            ID_CAPTURE_TYPE,
            180,
            84,
            280,
            140,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | CBS_DROPDOWNLIST as u32),
            WINDOW_EX_STYLE::default(),
        );
        add_combo_item(state.capture_combo, &state.i18n.tr("config.type.mumu"));
        add_combo_item(state.capture_combo, &state.i18n.tr("config.type.ldplayer"));
        add_combo_item(state.capture_combo, &state.i18n.tr("config.type.minicap"));
        add_combo_item(state.capture_combo, &state.i18n.tr("config.type.window"));
        let _ = SendMessageW(state.capture_combo, CB_SETCURSEL, WPARAM(0), LPARAM(0));

        state.dynamic_group = create_control(
            hwnd,
            "BUTTON",
            &state.i18n.tr("config.mumu.frame.title"),
            ID_DYNAMIC_GROUP,
            20,
            124,
            570,
            190,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | BS_GROUPBOX as u32),
            WINDOW_EX_STYLE::default(),
        );
        state.path_label = create_static_id(
            hwnd,
            ID_PATH_LABEL,
            40,
            154,
            120,
            22,
            &state.i18n.tr("config.label.path"),
        );
        state.path_edit = create_control(
            hwnd,
            "EDIT",
            "",
            ID_PATH_EDIT,
            170,
            150,
            300,
            24,
            WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32 | WS_BORDER.0,
            ),
            WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0),
        );
        state.browse_button = create_control(
            hwnd,
            "BUTTON",
            &state.i18n.tr("config.btn.browse"),
            ID_BROWSE,
            480,
            149,
            90,
            26,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        );
        state.instance_label = create_static_id(
            hwnd,
            ID_INSTANCE_LABEL,
            40,
            190,
            120,
            22,
            &state.i18n.tr("config.label.instance"),
        );
        state.instance_edit = create_control(
            hwnd,
            "EDIT",
            "0",
            ID_INSTANCE_EDIT,
            170,
            186,
            90,
            24,
            WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32 | WS_BORDER.0,
            ),
            WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0),
        );
        state.device_label = create_static_id(
            hwnd,
            ID_DEVICE_LABEL,
            40,
            226,
            240,
            22,
            &state.i18n.tr("config.label.adb_id_optional"),
        );
        state.device_edit = create_control(
            hwnd,
            "EDIT",
            "",
            ID_DEVICE_EDIT,
            280,
            222,
            290,
            24,
            WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32 | WS_BORDER.0,
            ),
            WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0),
        );
        state.scan_button = create_control(
            hwnd,
            "BUTTON",
            &state.i18n.tr("config.window.btn.scan"),
            ID_SCAN,
            40,
            154,
            130,
            28,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        );
        state.window_list = create_control(
            hwnd,
            "LISTBOX",
            "",
            ID_WINDOW_LIST,
            40,
            190,
            530,
            84,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_BORDER.0 | WS_VSCROLL.0),
            WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0),
        );
        state.selected_label = create_static_id(
            hwnd,
            ID_SELECTED_LABEL,
            40,
            282,
            530,
            22,
            &state.i18n.tr("config.window.scan.none"),
        );

        state.save_button = create_control(
            hwnd,
            "BUTTON",
            &state.i18n.tr("config.btn.save_start"),
            ID_SAVE,
            350,
            335,
            140,
            34,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        );
        create_control(
            hwnd,
            "BUTTON",
            "Cancel",
            IDCANCEL.0,
            500,
            335,
            90,
            34,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        );
    }

    unsafe fn update_dynamic_controls(state: &mut WizardState) {
        let choice = capture_choice(state);
        let title = match choice {
            CaptureChoice::Mumu => state.i18n.tr("config.mumu.frame.title"),
            CaptureChoice::LdPlayer => state.i18n.tr("config.ldplayer.frame.title"),
            CaptureChoice::Minicap => state.i18n.tr("config.minicap.frame.title"),
            CaptureChoice::Window => state.i18n.tr("config.window.frame.title"),
        };
        set_text(state.dynamic_group, &title);

        let path_visible = matches!(choice, CaptureChoice::Mumu | CaptureChoice::LdPlayer);
        let instance_visible = path_visible;
        let device_visible = !matches!(choice, CaptureChoice::Window);
        let window_visible = matches!(choice, CaptureChoice::Window);

        show_many(
            &[state.path_label, state.path_edit, state.browse_button],
            path_visible,
        );
        show_many(
            &[state.instance_label, state.instance_edit],
            instance_visible,
        );
        show_many(&[state.device_label, state.device_edit], device_visible);
        show_many(
            &[state.scan_button, state.window_list, state.selected_label],
            window_visible,
        );

        let device_label = if matches!(choice, CaptureChoice::Minicap) {
            state.i18n.tr("config.label.adb_id_auto")
        } else {
            state.i18n.tr("config.label.adb_id_optional")
        };
        set_text(state.device_label, &device_label);
    }

    unsafe fn scan_windows_into_state(state: &mut WizardState) {
        state.candidates = scan_windows();
        state.selected_window = None;
        let _ = SendMessageW(state.window_list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
        for candidate in &state.candidates {
            let label = format!("{} [{}]", candidate.title, candidate.class_name);
            let label = wide(&label);
            let _ = SendMessageW(
                state.window_list,
                LB_ADDSTRING,
                WPARAM(0),
                LPARAM(label.as_ptr() as isize),
            );
        }

        if state.candidates.is_empty() {
            set_text(
                state.selected_label,
                &state.i18n.tr("config.window.scan.none"),
            );
        } else {
            let _ = SendMessageW(state.window_list, LB_SETCURSEL, WPARAM(0), LPARAM(0));
            state.selected_window = Some(0);
            let message = state.i18n.tr_with(
                "config.window.scan.found",
                &[("count", state.candidates.len().to_string())],
            );
            set_text(state.selected_label, &message);
        }
    }

    unsafe fn select_window_from_list(state: &mut WizardState) {
        let selected = SendMessageW(state.window_list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        if selected < 0 {
            state.selected_window = None;
            return;
        }
        let index = selected as usize;
        state.selected_window = Some(index);
        if let Some(candidate) = state.candidates.get(index) {
            let message = state.i18n.tr_with(
                "config.window.selected",
                &[("title", candidate.title.clone())],
            );
            set_text(state.selected_label, &message);
        }
    }

    unsafe fn browse_install_path(state: &mut WizardState) {
        let title_key = match capture_choice(state) {
            CaptureChoice::LdPlayer => "config.browse.title.ldplayer",
            _ => "config.browse.title.mumu",
        };
        if let Some(path) = browse_folder(state.hwnd, &state.i18n.tr(title_key)) {
            set_text(state.path_edit, &path);
        }
    }

    unsafe fn save_config(state: &mut WizardState) {
        let choice = capture_choice(state);
        let path = edit_text(state.path_edit).trim().to_string();
        if matches!(choice, CaptureChoice::Mumu | CaptureChoice::LdPlayer) && path.is_empty() {
            let (title_key, message_key) = if matches!(choice, CaptureChoice::Mumu) {
                (
                    "config.error.mumu_path_empty.title",
                    "config.error.mumu_path_empty",
                )
            } else {
                (
                    "config.error.ld_path_empty.title",
                    "config.error.ld_path_empty",
                )
            };
            show_error(
                state.hwnd,
                &state.i18n.tr(title_key),
                &state.i18n.tr(message_key),
            );
            return;
        }

        let selected_window = if matches!(choice, CaptureChoice::Window) {
            if state.candidates.is_empty() {
                scan_windows_into_state(state);
            }
            let index = state.selected_window;
            let Some(index) = index else {
                show_error(
                    state.hwnd,
                    &state
                        .i18n
                        .tr("config.window.error.no_window_selected.title"),
                    &state.i18n.tr("config.window.error.no_window_selected"),
                );
                return;
            };
            Some(state.candidates[index].clone())
        } else {
            None
        };

        let capture_type = match choice {
            CaptureChoice::Mumu => "mumu",
            CaptureChoice::LdPlayer => "ldplayer",
            CaptureChoice::Minicap => "minicap",
            CaptureChoice::Window => "window",
        };
        let language = if combo_index(state.language_combo) == 1 {
            "en_US"
        } else {
            "zh_CN"
        };
        let instance_index = edit_text(state.instance_edit)
            .trim()
            .parse::<u32>()
            .unwrap_or(0);
        let device_id = empty_to_none(edit_text(state.device_edit));

        state.result = Some(RulerConfig {
            capture_type: capture_type.to_string(),
            install_path: if path.is_empty() { None } else { Some(path) },
            instance_index: if matches!(choice, CaptureChoice::Mumu | CaptureChoice::LdPlayer) {
                Some(instance_index)
            } else {
                None
            },
            device_id,
            window_handle: selected_window.as_ref().map(|candidate| candidate.hwnd),
            window_title: selected_window
                .as_ref()
                .map(|candidate| candidate.title.clone()),
            window_class: selected_window
                .as_ref()
                .map(|candidate| candidate.class_name.clone()),
            active_calibration_profile: None,
            frame_display_mode: Some("0_to_n-1".to_string()),
            language: Some(language.to_string()),
        });
        state.done = true;
        let _ = DestroyWindow(state.hwnd);
    }

    unsafe fn close_without_result(state: &mut WizardState) {
        state.result = None;
        state.done = true;
        let _ = DestroyWindow(state.hwnd);
    }

    unsafe fn capture_choice(state: &WizardState) -> CaptureChoice {
        match combo_index(state.capture_combo) {
            1 => CaptureChoice::LdPlayer,
            2 => CaptureChoice::Minicap,
            3 => CaptureChoice::Window,
            _ => CaptureChoice::Mumu,
        }
    }

    unsafe fn combo_index(hwnd: HWND) -> usize {
        let value = SendMessageW(hwnd, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        if value < 0 {
            0
        } else {
            value as usize
        }
    }

    unsafe fn scan_windows() -> Vec<WindowCandidate> {
        let mut windows = Vec::new();
        let _ = EnumWindows(
            Some(enum_window_proc),
            LPARAM((&mut windows as *mut Vec<WindowCandidate>) as isize),
        );
        windows.sort_by_key(window_priority);
        windows
    }

    unsafe extern "system" fn enum_window_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        let title = window_text(hwnd);
        if title.trim().is_empty() {
            return BOOL(1);
        }
        let class_name = window_class(hwnd);
        let windows = &mut *(lparam.0 as *mut Vec<WindowCandidate>);
        windows.push(WindowCandidate {
            hwnd: hwnd.0 as isize,
            title,
            class_name,
        });
        BOOL(1)
    }

    fn window_priority(candidate: &WindowCandidate) -> (u8, String) {
        let title = candidate.title.to_ascii_lowercase();
        let class_name = candidate.class_name.to_ascii_lowercase();
        let priority = if candidate.title == "明日方舟" {
            0
        } else if candidate.title.contains("明日方舟") {
            1
        } else if title.contains("arknights") {
            2
        } else if candidate.title.contains("方舟") {
            3
        } else if class_name == "unitywndclass" || class_name == "unityhwndclass" {
            4
        } else {
            5
        };
        (priority, candidate.title.clone())
    }

    unsafe fn window_text(hwnd: HWND) -> String {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let count = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..count as usize])
    }

    unsafe fn window_class(hwnd: HWND) -> String {
        let mut buf = [0u16; 256];
        let count = GetClassNameW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..count as usize])
    }

    unsafe fn browse_folder(owner: HWND, title: &str) -> Option<String> {
        let title = wide(title);
        let mut display_name = [0u16; 260];
        let browse_info = BROWSEINFOW {
            hwndOwner: owner,
            pszDisplayName: PWSTR(display_name.as_mut_ptr()),
            lpszTitle: PCWSTR(title.as_ptr()),
            ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
            ..Default::default()
        };
        let pidl = SHBrowseForFolderW(&browse_info);
        if pidl.is_null() {
            return None;
        }

        let mut path = [0u16; 260];
        let ok = SHGetPathFromIDListW(pidl, &mut path).as_bool();
        CoTaskMemFree(Some(pidl as *const c_void));
        if !ok {
            return None;
        }
        Some(trim_nul(&path))
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

    unsafe fn add_combo_item(combo: HWND, text: &str) {
        let text = wide(text);
        let _ = SendMessageW(
            combo,
            CB_ADDSTRING,
            WPARAM(0),
            LPARAM(text.as_ptr() as isize),
        );
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

    unsafe fn show_many(windows: &[HWND], visible: bool) {
        for hwnd in windows {
            let _ = ShowWindow(*hwnd, if visible { SW_SHOW } else { SW_HIDE });
        }
    }

    unsafe fn edit_text(hwnd: HWND) -> String {
        let len = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; len.max(0) as usize + 1];
        let count = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..count as usize])
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

    fn empty_to_none(value: String) -> Option<String> {
        let value = value.trim().to_string();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    }

    fn child_id(id: i32) -> HMENU {
        if id == 0 {
            HMENU::default()
        } else {
            HMENU(id as isize as *mut c_void)
        }
    }

    fn trim_nul(buf: &[u16]) -> String {
        let end = buf
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(iter::once(0)).collect()
    }
}
