//! The HUD overlay window: a layered, top-most, click-through-where-empty
//! window that software-renders `hud.slint`, owns the message loop and the
//! 16 ms repaint timer, mirrors `SharedAppState` into HUD properties, and
//! handles pointer drag / tray plumbing. Its right-click opens the [`super::menu`].

use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        atomic::{AtomicIsize, Ordering},
        mpsc::Sender,
        Arc,
    },
    time::Instant,
};

use slint::{
    platform::{
        software_renderer::{MinimalSoftwareWindow, SoftwareRenderer},
        PointerEventButton, WindowAdapter, WindowEvent,
    },
    ComponentHandle, LogicalPosition, PhysicalSize,
};

use super::geometry::{
    advance_displayed_progress, compute_reset_phase, initial_geometry,
    outer_area_should_pass_through_at,
};
use super::{menu, LOGICAL_H, LOGICAL_W, OVERLAY_TIMER_INTERVAL_MS};
use crate::overlay::OverlayError;
use crate::{
    commands::UiCommand,
    i18n::I18n,
    icons::IconSet,
    slint_win::{
        create_dib, ensure_platform, logical_pos, present_layered, wide, PreBgra, WindowSlot,
    },
    tray,
    ui::{Hud, HudMode},
    ui_state::{OverlayMode, ResetKind},
    worker::SharedAppState,
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::WM_MOUSELEAVE,
            Input::KeyboardAndMouse::{
                ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
            },
            Shell::NOTIFYICONDATAW,
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
                GetMessageW, GetWindowLongPtrW, GetWindowRect, LoadCursorW, PostMessageW,
                PostQuitMessage, RegisterClassW, SetTimer, SetWindowLongPtrW, SetWindowPos,
                ShowWindow, TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA,
                HICON, HMENU, IDC_ARROW, MSG, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_SHOW,
                WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP,
                WM_MOUSEMOVE, WM_NCCREATE, WM_NCHITTEST, WM_PAINT, WM_RBUTTONUP, WM_TIMER,
                WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
            },
        },
    },
};

const OVERLAY_TIMER_ID: usize = 1;
const WM_OVERLAY_WAKE: u32 = WM_APP + 2;
const WM_TRAYICON: u32 = WM_APP + 1;
const HTTRANSPARENT_RESULT: isize = -1;

/// In-flight reset cover animation. `kind` selects the label text.
#[derive(Clone, Copy)]
struct ResetAnim {
    start: Instant,
    kind: ResetKind,
}

pub(super) struct WindowState {
    hud: Hud,
    window: Rc<MinimalSoftwareWindow>,
    pub(super) state: Arc<SharedAppState>,
    pub(super) command_tx: Sender<UiCommand>,
    pub(super) i18n: Arc<I18n>,
    // software framebuffer backed by a DIB section (no copy on present)
    mem_dc: HDC,
    dib: HBITMAP,
    bits: *mut PreBgra,
    buf_w: usize,
    buf_h: usize,
    pub(super) scale: f32,
    base_scale: f32,
    scale_mult: f32,
    displayed_progress: f32,
    last_mode: OverlayMode,
    // Reset cover animation: detects new resets via `reset_pulse` and plays
    // the cover sweep. `None` when idle.
    reset_anim: Option<ResetAnim>,
    last_reset_pulse: u32,
    // factory slot, used to claim windows for menu/dialog popups
    pub(super) window_slot: WindowSlot,
    // tray icon bound to this window (Shell_NotifyIcon)
    nid: NOTIFYICONDATAW,
    tray_icon: Option<HICON>,
    // guards against a re-entrant popup (e.g. tray click while menu is open)
    pub(super) menu_open: bool,
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

pub(crate) fn run(
    shared_state: Arc<SharedAppState>,
    command_tx: Sender<UiCommand>,
    i18n: Arc<I18n>,
    icons: Arc<IconSet>,
    placement: crate::overlay::OverlayPlacement,
) -> Result<(), OverlayError> {
    // Shared, idempotent platform init: the config wizard may have already
    // installed it (set_platform is once-per-process). NewBuffer repaint and
    // bundled fonts are configured there.
    let slot: WindowSlot = ensure_platform();

    let hud = Hud::new()
        .map_err(|error| OverlayError::new(format!("failed to build HUD component: {error}")))?;
    let window = slot
        .borrow_mut()
        .take()
        .ok_or_else(|| OverlayError::new("platform did not produce a HUD window"))?;

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
    wire_callbacks(&hud, &shared_state, &command_tx, &drag_on_bg, &hwnd_cell);

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
            displayed_progress: 0.0,
            last_mode: OverlayMode::Booting,
            reset_anim: None,
            last_reset_pulse: 0,
            window_slot: Rc::clone(&slot),
            nid: NOTIFYICONDATAW::default(),
            tray_icon: None,
            menu_open: false,
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

        // Tray icon now lives on the UI thread alongside the HUD, so its
        // right-click can open the same Slint menu.
        if let Some((nid, tray_icon)) = tray::win32::install(hwnd, &icons, WM_TRAYICON) {
            if let Some(state) = window_state_mut(hwnd) {
                state.nid = nid;
                state.tray_icon = tray_icon;
            }
        }

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
    hud.on_undo_reset_clicked({
        let tx = command_tx.clone();
        move || {
            let _ = tx.send(UiCommand::UndoResetTimer);
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
        WM_NCHITTEST => {
            if let Some(state) = window_state_mut(hwnd) {
                if outer_area_should_pass_through(hwnd, state) {
                    return LRESULT(HTTRANSPARENT_RESULT);
                }
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_TIMER => {
            if should_exit(hwnd) {
                let _ = DestroyWindow(hwnd);
            } else {
                tick(hwnd);
            }
            LRESULT(0)
        }
        WM_OVERLAY_WAKE => {
            // Worker notifications can arrive at ~1 kHz; rendering is
            // throttled to the 16 ms timer, so only act on a prompt exit.
            if should_exit(hwnd) {
                let _ = DestroyWindow(hwnd);
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
            menu::show_menu(hwnd);
            LRESULT(0)
        }
        WM_TRAYICON => {
            // Shell forwards the mouse action in the low word of lparam.
            if lparam.0 as u32 == WM_RBUTTONUP {
                if let Some(state) = window_state_mut(hwnd) {
                    let snapshot = state.state.snapshot();
                    let tip = match snapshot.ui.mode {
                        OverlayMode::Running => "明日方舟费用条尺子".to_string(),
                        _ => snapshot.ui.message.clone(),
                    };
                    tray::win32::update_tooltip(&mut state.nid, &tip);
                }
                menu::show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
            if !state_ptr.is_null() {
                (*state_ptr).state.set_overlay_waker(None);
                tray::win32::remove(&(*state_ptr).nid, (*state_ptr).tray_icon);
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
    advance_reset_anim(state, &snapshot.ui);

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

fn sync_properties(state: &mut WindowState, ui: &crate::ui_state::UiSnapshot) {
    let displayed_progress = displayed_calibration_progress(state, ui);
    let progress_text = displayed_progress.round().clamp(0.0, 100.0) as u8;

    {
        let hud = &state.hud;

        hud.set_mode(map_mode(&ui.mode));
        hud.set_time_str(ui.time_str.as_str().into());
        hud.set_frame_str(ui.display_frame.as_str().into());
        hud.set_undo_reset_enabled(ui.can_undo_reset);

        let negative = ui.display_total.ends_with('*');
        let total_clean = ui.display_total.trim_end_matches('*');
        hud.set_total_str(total_clean.into());
        hud.set_cost_negative(negative);

        let lap = ui
            .lap_frames
            .map(|frames| frames.to_string())
            .unwrap_or_default();
        hud.set_lap_str(lap.into());

        hud.set_progress(displayed_progress);
        hud.set_progress_str(format!("{progress_text}%").into());

        hud.set_cursor_blocked(ui.cursor_blocked);
        hud.set_cursor_warning_text(state.i18n.tr("overlay.cursor.blocked").into());

        let message = match ui.mode {
            OverlayMode::Idle => state.i18n.tr("overlay.msg.idle"),
            OverlayMode::PreCalibration => state.i18n.tr("overlay.msg.pre_cal"),
            OverlayMode::Error | OverlayMode::Booting => ui.message.clone(),
            _ => String::new(),
        };
        hud.set_message(message.into());
    }

    state.last_mode = ui.mode.clone();
}

/// Detect new resets via `reset_pulse` and advance the cover animation.
/// A fresh pulse mid-animation restarts it (overriding the current kind),
/// so a manual reset right after an auto one re-arms cleanly.
fn advance_reset_anim(state: &mut WindowState, ui: &crate::ui_state::UiSnapshot) {
    if ui.reset_pulse != state.last_reset_pulse {
        state.last_reset_pulse = ui.reset_pulse;
        // Always (re)start from the top so the sweep reads as a new event.
        state.reset_anim = Some(ResetAnim {
            start: Instant::now(),
            kind: ui.reset_kind,
        });
    }

    let Some(anim) = state.reset_anim else {
        state.hud.set_reset_active(false);
        return;
    };

    let elapsed = anim.start.elapsed().as_millis();
    let phase = compute_reset_phase(elapsed);
    let hud = &state.hud;
    if phase.done {
        state.reset_anim = None;
        hud.set_reset_active(false);
        return;
    }

    let label_key = match anim.kind {
        ResetKind::Manual => "overlay.reset.manual",
        ResetKind::Auto => "overlay.reset.auto",
    };
    hud.set_reset_active(true);
    hud.set_reset_cover(phase.cover);
    hud.set_reset_top(phase.top);
    hud.set_reset_text(state.i18n.tr(label_key).into());
    hud.set_reset_text_opacity(phase.text_opacity);
}

fn displayed_calibration_progress(
    state: &mut WindowState,
    ui: &crate::ui_state::UiSnapshot,
) -> f32 {
    if ui.mode != OverlayMode::Calibrating {
        state.displayed_progress = 0.0;
        return 0.0;
    }

    let target = ui.progress_percent.clamp(0.0, 100.0);
    if state.last_mode != OverlayMode::Calibrating || target <= 0.0 {
        state.displayed_progress = 0.0;
    }
    if target >= 99.9 {
        state.displayed_progress = 100.0;
        return state.displayed_progress;
    }

    state.displayed_progress = advance_displayed_progress(state.displayed_progress, target);
    state.displayed_progress
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
    let position =
        LogicalPosition::new(client.x as f32 / state.scale, client.y as f32 / state.scale);

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

unsafe fn should_exit(hwnd: HWND) -> bool {
    window_state(hwnd)
        .map(|state| state.state.snapshot().ui.should_exit)
        .unwrap_or(false)
}

pub(super) unsafe fn window_state<'a>(hwnd: HWND) -> Option<&'a WindowState> {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
    if state_ptr.is_null() {
        None
    } else {
        Some(&*state_ptr)
    }
}

pub(super) unsafe fn window_state_mut<'a>(hwnd: HWND) -> Option<&'a mut WindowState> {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
    if state_ptr.is_null() {
        None
    } else {
        Some(&mut *state_ptr)
    }
}

unsafe fn outer_area_should_pass_through(hwnd: HWND, state: &mut WindowState) -> bool {
    let mut cursor = POINT::default();
    if GetCursorPos(&mut cursor).is_err() {
        return false;
    }
    let mut window_rect = RECT::default();
    if GetWindowRect(hwnd, &mut window_rect).is_err() {
        return false;
    }

    let x = cursor.x - window_rect.left;
    let y = cursor.y - window_rect.top;
    let running = matches!(state.state.snapshot().ui.mode, OverlayMode::Running);
    outer_area_should_pass_through_at(x, y, state.scale, running)
}
