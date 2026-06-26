//! The right-click context-menu popup: a second layered window that
//! software-renders `menu.slint` in its own nested message loop, driven from
//! the HUD's [`super::hud`] window. Also hosts the inline-rename keyboard
//! plumbing (WM_CHAR / WM_KEYDOWN → Slint key events).

use std::{
    cell::Cell,
    rc::Rc,
    sync::{mpsc::Sender, Arc},
};

use slint::{
    platform::{
        software_renderer::{MinimalSoftwareWindow, SoftwareRenderer},
        Key, PointerEventButton, WindowAdapter, WindowEvent,
    },
    ComponentHandle, ModelRc, PhysicalSize, VecModel,
};

use super::hud::{window_state, window_state_mut};
use super::OVERLAY_TIMER_INTERVAL_MS;
use crate::{
    commands::UiCommand,
    i18n::I18n,
    slint_win::{
        client_xy, create_dib, decode_wm_char, dispatch_key, logical_pos, present_layered, wide,
        PreBgra,
    },
    ui::{ProfileRow, RulerMenu},
    ui_state::{FrameDisplayMode, OverlayMode},
    worker::SharedAppState,
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::{DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{
                ReleaseCapture, SetCapture, VK_BACK, VK_DELETE, VK_END, VK_ESCAPE, VK_HOME,
                VK_LEFT, VK_RETURN, VK_RIGHT,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
                GetMessageW, GetSystemMetrics, GetWindowLongPtrW, LoadCursorW, PostQuitMessage,
                RegisterClassW, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos,
                ShowWindow, TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA,
                HMENU, IDC_ARROW, MSG, SM_CXSCREEN, SM_CYSCREEN, SWP_NOMOVE, SWP_NOZORDER, SW_SHOW,
                WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN,
                WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_RBUTTONDOWN, WM_TIMER, WNDCLASSW,
                WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
            },
        },
    },
};

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

pub(super) unsafe fn show_menu(parent: HWND) {
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
    let calibrating = snapshot.ui.mode == OverlayMode::Calibrating;
    // While calibrating the profile list is hidden (see `populate_menu`),
    // so the popup is sized for zero profile rows.
    let effective_profiles: &[crate::ui_state::ProfileMenuItem] = if calibrating {
        &[]
    } else {
        &snapshot.ui.profiles
    };
    let n_profiles = effective_profiles.len();

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
        profiles_sig: profiles_signature(effective_profiles),
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
    let calibrating = ui.mode == OverlayMode::Calibrating;
    menu.set_calibrating(calibrating);
    // Hide profile rows while calibrating: clicking one would queue a
    // UseProfile that the blocked worker can't act on until the run ends,
    // and the user's intent during calibration is to cancel, not switch.
    if calibrating {
        rebuild_profiles(menu, &[]);
    } else {
        rebuild_profiles(menu, &ui.profiles);
    }
    menu.set_editing_index(-1);
    menu.set_deleting_index(-1);
    menu.set_display_mode(display_mode_index(ui.display_mode));
    menu.set_scale_index(scale_pct_to_index(ui.overlay_scale_pct));
    menu.set_timer_enabled(ui.active_profile.is_some());
    menu.set_undo_reset_enabled(ui.can_undo_reset);
    menu.set_cap_calibration(i18n.tr("overlay.menu.calibration").into());
    menu.set_cap_calibrating(i18n.tr("overlay.menu.calibrating").into());
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
    menu.set_update_available(ui.update_notice.is_some());
    menu.set_update_text(i18n.tr("update.badge.menu").into());
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
    // We also push the change straight into the shared state so the chips
    // update (and the overlay resizes) even when the worker is blocked
    // inside the calibration capture loop — the queued command just
    // persists the same value once the worker is free.
    menu.on_set_display({
        let tx = command_tx.clone();
        let state = Arc::clone(state);
        move |index| {
            let mode = index_to_display_mode(index);
            state.update_ui(|ui, _| ui.display_mode = mode);
            let _ = tx.send(UiCommand::SetDisplayMode(mode));
        }
    });
    menu.on_set_scale({
        let tx = command_tx.clone();
        let state = Arc::clone(state);
        move |index| {
            let mult = index_to_scale(index);
            let pct = (mult * 100.0).round().clamp(50.0, 400.0) as u16;
            state.update_ui(|ui, _| ui.overlay_scale_pct = pct);
            let _ = tx.send(UiCommand::SetOverlayScale(mult));
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
        let state = Arc::clone(state);
        let closing = Rc::clone(closing);
        move || {
            let url = state
                .snapshot()
                .ui
                .update_notice
                .map(|notice| notice.html_url);
            unsafe {
                if let Some(url) = url {
                    crate::menu::win32::open_url(&url);
                } else {
                    crate::menu::win32::open_about_page();
                }
            }
            closing.set(true);
        }
    });
    // Cancel an in-flight calibration: set the one-shot cancel flag the
    // worker polls in its capture loop, then close the menu. The worker
    // returns to PreCalibration on its own; the user can retry immediately.
    menu.on_cancel_calibration({
        let state = Arc::clone(state);
        let closing = Rc::clone(closing);
        move || {
            state.request_cancel_calibration();
            closing.set(true);
        }
    });
    menu.on_exit({
        let tx = command_tx.clone();
        let state = Arc::clone(state);
        let closing = Rc::clone(closing);
        move || {
            // Abort any in-flight calibration first so the worker unblocks
            // and drains the queued Exit promptly. Without this the worker
            // is stuck inside `collect_calibration_samples` and the Exit
            // command would not be processed until the run finished.
            state.request_cancel_calibration();
            let _ = tx.send(UiCommand::Exit);
            closing.set(true);
        }
    });
}

// ===================== inline-edit keyboard plumbing =====================

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
                let inside = x >= 0 && y >= 0 && x < state.buf_w as i32 && y < state.buf_h as i32;
                if !inside {
                    state.closing.set(true);
                } else if message == WM_LBUTTONDOWN {
                    let position = logical_pos(lparam, state.scale);
                    let _ = state
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
                let inside = x >= 0 && y >= 0 && x < state.buf_w as i32 && y < state.buf_h as i32;
                if inside {
                    let position = logical_pos(lparam, state.scale);
                    let _ =
                        state
                            .window
                            .window()
                            .try_dispatch_event(WindowEvent::PointerReleased {
                                position,
                                button: PointerEventButton::Left,
                            });
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

    // Sync the calibrating flag every tick so the header swaps between the
    // "New" and "Cancel" buttons if the worker transitions modes while the
    // menu is open (e.g. calibration finishes or is cancelled mid-menu).
    let calibrating = snapshot.ui.mode == OverlayMode::Calibrating;
    state.menu.set_calibrating(calibrating);

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
    state
        .menu
        .set_update_available(snapshot.ui.update_notice.is_some());

    // While calibrating, force the effective profile list to empty so the
    // `for` loop in menu.slint renders zero rows and the popup can shrink.
    let effective_profiles: Vec<_> = if calibrating {
        Vec::new()
    } else {
        snapshot.ui.profiles.clone()
    };

    // Refresh the profile model only when the list actually changes (an inline
    // rename/delete landed, or the calibrating state flipped), so an in-progress
    // edit field is not torn down each frame. Resize the popup when the row
    // count changed.
    let sig = profiles_signature(&effective_profiles);
    if state.profiles_sig != sig {
        rebuild_profiles(&state.menu, &effective_profiles);
        state.profiles_sig = sig;
        let new_count = effective_profiles.len();
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
