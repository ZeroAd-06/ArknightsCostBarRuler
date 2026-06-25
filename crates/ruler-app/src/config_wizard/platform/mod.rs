//! Windows config wizard: a layered, draggable setup window that software-
//! renders `wizard.slint`, discovers capture targets, live-probes each one,
//! and returns the chosen [`RulerConfig`].
//!
//! Split by responsibility:
//! - [`callbacks`] caption population, Slint callbacks, manual-config builder,
//!   native file picker
//! - [`sync`]      pushing [`WizardCore`] state into the Slint component +
//!   preview pixel conversion
//! - [`probe`]     target discovery and the background probe workers
//!
//! This module owns the shared state ([`WizardCore`]), the HWND-bound
//! [`WizardWindow`], the lifecycle entry point, and the native message loop.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};

use ruler_core::RulerConfig;
use slint::{
    platform::{
        software_renderer::{MinimalSoftwareWindow, SoftwareRenderer},
        Key, PointerEventButton, WindowAdapter, WindowEvent,
    },
    ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel,
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{
                ReleaseCapture, SetCapture, VK_BACK, VK_DELETE, VK_END, VK_ESCAPE, VK_HOME,
                VK_LEFT, VK_RETURN, VK_RIGHT,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
                GetMessageW, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, LoadCursorW,
                RegisterClassW, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos,
                ShowWindow, TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA,
                HMENU, IDC_ARROW, MSG, SM_CXSCREEN, SM_CYSCREEN, SWP_NOMOVE, SWP_NOSIZE,
                SWP_NOZORDER, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR, WM_DESTROY,
                WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE,
                WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
            },
        },
    },
};

use crate::{
    i18n::I18n,
    slint_win::{create_dib, ensure_platform, logical_pos, present_layered, wide, PreBgra},
    target_discovery::TargetCandidate,
    ui::{TargetRow, Wizard},
};

mod callbacks;
mod probe;
mod sync;

use callbacks::{populate_captions, wire_callbacks, BrowseAction};
use probe::{
    drain_probe_messages, re_resolve_adb, refresh_candidates, stop_probe_worker_list, ProbeMessage,
    ProbeWorker,
};
use sync::{preview_cap_for_scale, sync_to_slint, wizard_logical_height};

const CLASS_NAME: &str = "RulerWizardWindowClass";
const TIMER_ID: usize = 1;
const TICK_INTERVAL_MS: u32 = 16;
// Mouse-wheel scrolling for the target list. One wheel notch is
// `WHEEL_DELTA` (120) raw units; map each notch to `WHEEL_STEP_LOGICAL_PX`
// logical pixels of Flickable travel, mirroring Slint's own backends
// (~60 logical px per line) so the list scrolls at a familiar speed.
const WHEEL_DELTA_UNIT: f32 = 120.0;
const WHEEL_STEP_LOGICAL_PX: f32 = 60.0;
// Fixed logical design width of `wizard.slint` (physical = logical * scale).
const WIZARD_LOGICAL_W: f32 = 560.0;

/// All mutable wizard data shared between the Slint callbacks and the render
/// tick. Both run on the single UI thread, so a plain `Rc<RefCell<_>>` is safe.
struct WizardCore {
    i18n: I18n,
    previous_config: Option<RulerConfig>,
    candidates: Vec<TargetCandidate>,
    selected_index: Option<usize>,
    // When the trailing "manual" list row is active the candidate path is
    // bypassed: the target is built from typed fields (held in the Slint
    // in-out properties) on confirm. `selected_index` is `None` while this
    // is set, so no candidate is highlighted or probed for the selection.
    manual_mode: bool,
    // Set when a manual confirm fails validation; rendered on the status line
    // until the selection or fields change.
    manual_error: Option<String>,
    probe_tx: Sender<ProbeMessage>,
    probe_rx: Receiver<ProbeMessage>,
    probe_generation: u64,
    probe_workers: Vec<ProbeWorker>,
    latency_samples: HashMap<String, VecDeque<Duration>>,
    // Persistent target-list model, updated in place between structural
    // changes (see `sync_rows`) so latency ticks don't tear down the
    // repeater and reset its row hover animations.
    rows_model: Rc<VecModel<TargetRow>>,
    // `None` until the first sync; reset to `None` on refresh to force a
    // re-evaluation. Holds the last pushed content signature otherwise.
    rows_content_sig: Option<String>,
    rows_struct_sig: String,
    // Physical pixel cap for the downscaled preview image (see `preview_image`).
    preview_cap: (u32, u32),
    // identity of the preview currently pushed to Slint: (selected idx, data ptr, len)
    preview_token: Option<(usize, usize, usize)>,
}

impl WizardCore {
    fn selected_candidate(&self) -> Option<&TargetCandidate> {
        self.selected_index
            .and_then(|index| self.candidates.get(index))
    }

    fn selected_fingerprint(&self) -> Option<String> {
        self.selected_candidate()
            .map(|candidate| candidate.fingerprint.clone())
    }
}

impl Drop for WizardCore {
    fn drop(&mut self) {
        stop_probe_worker_list(&mut self.probe_workers);
    }
}

/// The HWND-bound state: the Slint component, its software framebuffer, and a
/// handle to the shared [`WizardCore`].
struct WizardWindow {
    wizard: Wizard,
    window: Rc<MinimalSoftwareWindow>,
    core: Rc<RefCell<WizardCore>>,
    closing: Rc<Cell<bool>>,
    drag_on_title: Rc<Cell<bool>>,
    // Currently applied logical window height; the render tick recomputes the
    // target height (debug panel + manual panel toggles) and resizes on change.
    logical_h: f32,
    // Buffers the high half of a UTF-16 surrogate pair across WM_CHAR messages
    // while forwarding text input to the focused Slint TextInput.
    pending_high: Cell<u16>,
    mem_dc: HDC,
    dib: HBITMAP,
    bits: *mut PreBgra,
    buf_w: usize,
    buf_h: usize,
    scale: f32,
    mouse_down: bool,
    moved: bool,
    drag_origin: POINT,
    window_origin: POINT,
}

impl Drop for WizardWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.mem_dc);
            let _ = DeleteObject(HGDIOBJ(self.dib.0));
        }
    }
}

pub(super) fn run_config_wizard(
    i18n: &I18n,
    previous_config: Option<&RulerConfig>,
    debug: bool,
) -> Option<RulerConfig> {
    let slot = ensure_platform();
    let Ok(wizard) = Wizard::new() else {
        return None;
    };
    let window = slot.borrow_mut().take()?;

    // Computed once up front: drives both the native window size and the
    // physical pixel cap for the downscaled live preview.
    let scale = wizard_scale();

    // Persistent row model. The render tick updates it in place (a latency
    // tick changes only a couple of cells) instead of replacing it, so the
    // target list's repeater is not torn down — and its hover animations
    // not reset — several times a second.
    let rows_model: Rc<VecModel<TargetRow>> = Rc::new(VecModel::default());
    wizard.set_rows(ModelRc::from(rows_model.clone()));

    let (probe_tx, probe_rx) = mpsc::channel();
    let core = Rc::new(RefCell::new(WizardCore {
        i18n: i18n.clone(),
        previous_config: previous_config.cloned(),
        candidates: Vec::new(),
        selected_index: None,
        manual_mode: false,
        manual_error: None,
        probe_tx,
        probe_rx,
        probe_generation: 0,
        probe_workers: Vec::new(),
        latency_samples: HashMap::new(),
        rows_model,
        rows_content_sig: None,
        rows_struct_sig: String::new(),
        preview_cap: preview_cap_for_scale(scale),
        preview_token: None,
    }));
    let result: Rc<RefCell<Option<RulerConfig>>> = Rc::new(RefCell::new(None));
    let closing = Rc::new(Cell::new(false));
    let drag_on_title = Rc::new(Cell::new(false));
    // Browse buttons stage their file dialog here; the message loop runs it
    // outside the pointer handler's `&mut WizardWindow` borrow (see BrowseAction).
    let pending_browse: Rc<Cell<Option<BrowseAction>>> = Rc::new(Cell::new(None));

    populate_captions(&wizard, i18n, previous_config, debug);
    wire_callbacks(
        &wizard,
        &core,
        &result,
        &closing,
        &drag_on_title,
        &pending_browse,
    );

    // Resolve adb before the first discovery pass: the resolver cache is
    // consulted by every adb call site (AdbController, LDPlayerController,
    // AndroidInputOverlayGuard, target_discovery). Re-resolving here also
    // picks up any emulator started since `main.rs` did its initial probe.
    re_resolve_adb();

    // Initial discovery + probe.
    refresh_candidates(&mut core.borrow_mut());

    let width = (WIZARD_LOGICAL_W * scale).round() as i32;
    let logical_h = wizard_logical_height(debug, false);
    let height = (logical_h * scale).round() as i32;
    let _ = window
        .window()
        .try_dispatch_event(WindowEvent::ScaleFactorChanged {
            scale_factor: scale,
        });
    window.set_size(PhysicalSize::new(width as u32, height as u32));
    let _ = wizard.show();
    let _ = window
        .window()
        .try_dispatch_event(WindowEvent::WindowActiveChanged(true));

    unsafe {
        let Ok(module) = GetModuleHandleW(PCWSTR::null()) else {
            return None;
        };
        let class_name = wide(CLASS_NAME);
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: HINSTANCE(module.0),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);

        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let screen_h = GetSystemMetrics(SM_CYSCREEN);
        let left = ((screen_w - width) / 2).max(0);
        let top = ((screen_h - height) / 2).max(0);

        let (mem_dc, dib, bits) = create_dib(width, height)?;

        let state = Box::new(WizardWindow {
            wizard,
            window,
            core,
            closing: Rc::clone(&closing),
            drag_on_title,
            logical_h,
            pending_high: Cell::new(0),
            mem_dc,
            dib,
            bits,
            buf_w: width as usize,
            buf_h: height as usize,
            scale,
            mouse_down: false,
            moved: false,
            drag_origin: POINT::default(),
            window_origin: POINT::default(),
        });
        let state_ptr = Box::into_raw(state);

        let title = wide("Arknights Ruler Setup");
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_LAYERED.0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WINDOW_STYLE(WS_POPUP.0 | WS_VISIBLE.0),
            left,
            top,
            width,
            height,
            HWND::default(),
            HMENU::default(),
            HINSTANCE(module.0),
            Some(state_ptr.cast()),
        ) else {
            let _ = Box::from_raw(state_ptr);
            return None;
        };
        if hwnd.0.is_null() {
            let _ = Box::from_raw(state_ptr);
            return None;
        }

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetTimer(hwnd, TIMER_ID, TICK_INTERVAL_MS, None);
        tick(hwnd);

        let mut message = MSG::default();
        while !closing.get() {
            let value = GetMessageW(&mut message, HWND::default(), 0, 0).0;
            if value == 0 || value == -1 {
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
            // Run any browse dialog staged by a button this iteration. We are
            // back at the top level here, so no `&mut WizardWindow` is held
            // while the dialog pumps its own (re-entrant) message loop.
            if let Some(action) = pending_browse.take() {
                action();
            }
        }

        let _ = DestroyWindow(hwnd);
    }

    let config = result.borrow_mut().take();
    config
}

// ===================== render / sync tick =====================

unsafe fn tick(hwnd: HWND) {
    slint::platform::update_timers_and_animations();
    let Some(state) = wizard_window_mut(hwnd) else {
        return;
    };
    {
        let mut core = state.core.borrow_mut();
        drain_probe_messages(&mut core);
        sync_to_slint(&state.wizard, &mut core);
    }
    let debug_expanded = state.wizard.get_debug_expanded();
    let manual_mode = state.core.borrow().manual_mode;
    let want_h = wizard_logical_height(debug_expanded, manual_mode);
    if (want_h - state.logical_h).abs() > 0.5 {
        resize_wizard(hwnd, state, want_h);
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

unsafe fn resize_wizard(hwnd: HWND, state: &mut WizardWindow, logical_h: f32) {
    let width = state.buf_w as i32;
    let height = (logical_h * state.scale).round() as i32;
    let _ = DeleteDC(state.mem_dc);
    let _ = DeleteObject(HGDIOBJ(state.dib.0));
    if let Some((mem_dc, dib, bits)) = create_dib(width, height) {
        state.mem_dc = mem_dc;
        state.dib = dib;
        state.bits = bits;
        state.buf_h = height as usize;
    }
    state.logical_h = logical_h;
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

// ===================== window plumbing =====================

fn wizard_scale() -> f32 {
    unsafe { ((GetSystemMetrics(SM_CYSCREEN) as f32) / 720.0).clamp(1.25, 2.5) }
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
            let state_ptr = (*create_struct).lpCreateParams as *mut WizardWindow;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
            LRESULT(1)
        }
        WM_TIMER => {
            tick(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            handle_mouse_move(hwnd, lparam);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            handle_mouse_wheel(hwnd, wparam, lparam);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            handle_left_down(hwnd, lparam);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            handle_left_up(hwnd);
            LRESULT(0)
        }
        WM_CHAR => {
            handle_char(hwnd, wparam);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            handle_key_down(hwnd, wparam);
            LRESULT(0)
        }
        WM_DESTROY => {
            let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WizardWindow;
            if !state_ptr.is_null() {
                let _ = Box::from_raw(state_ptr);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe fn handle_left_down(hwnd: HWND, lparam: LPARAM) {
    let Some(state) = wizard_window_mut(hwnd) else {
        return;
    };
    let mut cursor = POINT::default();
    let _ = GetCursorPos(&mut cursor);
    let mut rect = RECT::default();
    let _ = GetWindowRect(hwnd, &mut rect);
    state.mouse_down = true;
    state.moved = false;
    state.drag_on_title.set(false);
    state.drag_origin = cursor;
    state.window_origin = POINT {
        x: rect.left,
        y: rect.top,
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
    let Some(state) = wizard_window_mut(hwnd) else {
        return;
    };
    if state.mouse_down && state.drag_on_title.get() {
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
            return;
        }
    }
    let position = logical_pos(lparam, state.scale);
    let _ = state
        .window
        .window()
        .try_dispatch_event(WindowEvent::PointerMoved { position });
}

unsafe fn handle_mouse_wheel(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
    let Some(state) = wizard_window_mut(hwnd) else {
        return;
    };
    // WM_MOUSEWHEEL packs a signed notch delta in the high word of wParam,
    // and — unlike WM_MOUSEMOVE — carries *screen* coordinates in lParam, so
    // round-trip through ScreenToClient before applying the window scale.
    let notches = (((wparam.0 >> 16) & 0xffff) as u16 as i16 as f32) / WHEEL_DELTA_UNIT;
    let mut point = POINT {
        x: (lparam.0 as u32 & 0xffff) as i16 as i32,
        y: ((lparam.0 as u32 >> 16) & 0xffff) as i16 as i32,
    };
    let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut point);
    let position =
        slint::LogicalPosition::new(point.x as f32 / state.scale, point.y as f32 / state.scale);
    // Positive delta_y moves the Flickable viewport toward the top, matching
    // a forward (away-from-user) wheel roll — the usual list convention.
    let _ = state
        .window
        .window()
        .try_dispatch_event(WindowEvent::PointerScrolled {
            position,
            delta_x: 0.0,
            delta_y: notches * WHEEL_STEP_LOGICAL_PX,
        });
}

unsafe fn handle_left_up(hwnd: HWND) {
    let Some(state) = wizard_window_mut(hwnd) else {
        return;
    };
    let moved = state.moved && state.drag_on_title.get();
    state.mouse_down = false;
    state.moved = false;
    state.drag_on_title.set(false);
    let _ = ReleaseCapture();

    if moved {
        // Finished a title-bar drag; cancel Slint's press grab so it is not
        // read as a click.
        let _ = state
            .window
            .window()
            .try_dispatch_event(WindowEvent::PointerExited);
        return;
    }
    let mut cursor = POINT::default();
    let _ = GetCursorPos(&mut cursor);
    let mut client = cursor;
    let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut client);
    let position =
        slint::LogicalPosition::new(client.x as f32 / state.scale, client.y as f32 / state.scale);
    let _ = state
        .window
        .window()
        .try_dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
}

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
/// the (stashed) high surrogate. Mirrors the menu's inline-rename input path.
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

/// Forward a typed character to the focused Slint `TextInput` (the manual-panel
/// parameter fields). Control codes arrive via WM_KEYDOWN instead.
unsafe fn handle_char(hwnd: HWND, wparam: WPARAM) {
    let Some(state) = wizard_window(hwnd) else {
        return;
    };
    if let Some(text) = decode_wm_char(&state.pending_high, wparam.0 as u16) {
        dispatch_key(&state.window, text);
    }
}

/// Esc closes the wizard; editing / navigation keys are forwarded to the
/// focused Slint `TextInput` (printable characters come through WM_CHAR).
unsafe fn handle_key_down(hwnd: HWND, wparam: WPARAM) {
    let Some(state) = wizard_window(hwnd) else {
        return;
    };
    let vk = wparam.0 as u16;
    if vk == VK_ESCAPE.0 {
        state.closing.set(true);
        return;
    }
    let key = match vk {
        v if v == VK_BACK.0 => Some(Key::Backspace),
        v if v == VK_DELETE.0 => Some(Key::Delete),
        v if v == VK_LEFT.0 => Some(Key::LeftArrow),
        v if v == VK_RIGHT.0 => Some(Key::RightArrow),
        v if v == VK_HOME.0 => Some(Key::Home),
        v if v == VK_END.0 => Some(Key::End),
        v if v == VK_RETURN.0 => Some(Key::Return),
        _ => None,
    };
    if let Some(key) = key {
        dispatch_key(&state.window, key.into());
    }
}

unsafe fn wizard_window<'a>(hwnd: HWND) -> Option<&'a WizardWindow> {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WizardWindow;
    (!state_ptr.is_null()).then(|| &*state_ptr)
}

unsafe fn wizard_window_mut<'a>(hwnd: HWND) -> Option<&'a mut WizardWindow> {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WizardWindow;
    (!state_ptr.is_null()).then(|| &mut *state_ptr)
}
