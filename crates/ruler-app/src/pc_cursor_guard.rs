use std::ffi::c_void;

use ruler_core::{
    analysis::{
        roi::{self, Roi},
        scanner::BattleState,
    },
    capture::WindowInfo,
};
use windows::Win32::Foundation::{BOOL, HWND, POINT};

const CURSOR_SHOWING: u32 = 0x0000_0001;

const REF_WIDTH: f64 = 1280.0;
const REF_HEIGHT: f64 = 720.0;
const REF_ASPECT_RATIO: f64 = REF_WIDTH / REF_HEIGHT;

const CURSOR_WIDTH_REF: f64 = 72.0;
const CURSOR_HEIGHT_REF: f64 = 81.0;
const CURSOR_HOTSPOT_X_REF: f64 = 8.0;
const CURSOR_HOTSPOT_Y_REF: f64 = 8.0;
const MIN_CURSOR_SIZE_FACTOR: f64 = 0.6;

const COST_SIGN_LEFT_OFFSET_FROM_RIGHT_REF: f64 = 70.0;
const COST_SIGN_RIGHT_OFFSET_FROM_RIGHT_REF: f64 = 42.0;
const COST_SIGN_TOP_OFFSET_FROM_BOTTOM_REF: f64 = 208.0;
const COST_SIGN_BOTTOM_OFFSET_FROM_BOTTOM_REF: f64 = 201.0;

#[repr(C)]
struct CursorInfo {
    cb_size: u32,
    flags: u32,
    h_cursor: *mut c_void,
    pt_screen_pos: POINT,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetCursorInfo(cursor_info: *mut CursorInfo) -> BOOL;
    fn GetCursorPos(point: *mut POINT) -> BOOL;
    fn WindowFromPoint(point: POINT) -> HWND;
    fn ScreenToClient(hwnd: HWND, point: *mut POINT) -> BOOL;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Rect {
    fn intersects(self, other: Self) -> bool {
        self.left < other.right
            && self.right > other.left
            && self.top < other.bottom
            && self.bottom > other.top
    }
}

#[derive(Debug)]
pub struct SelfDrawnCursorGuard {
    ui_scaler: f64,
    cursor_size: f64,
    ever_saw_hidden: bool,
    self_drawn_enabled: bool,
}

impl SelfDrawnCursorGuard {
    pub fn new(ui_scaler: f64, cursor_size: f64) -> Self {
        let ui_scaler = finite_or_default(ui_scaler, roi::DEFAULT_UI_SCALER).clamp(0.0, 1.0);
        let cursor_size = finite_or_default(cursor_size, 1.0).clamp(0.0, 1.0);
        Self {
            ui_scaler,
            cursor_size,
            ever_saw_hidden: false,
            self_drawn_enabled: false,
        }
    }

    pub fn should_pause_for_frame(
        &mut self,
        window: Option<WindowInfo>,
        roi: Option<Roi>,
        battle_state: BattleState,
    ) -> bool {
        // `NotInBattle` is the scanner's garbage / unreadable state. The cursor
        // warning is only useful when the frame is otherwise meaningful.
        if battle_state == BattleState::NotInBattle {
            return false;
        }

        let (Some(window), Some(roi)) = (window, roi) else {
            return false;
        };
        let Some(cursor) = cursor_state_in_client(window) else {
            return false;
        };

        // Only use Win32 visibility as a detector when the game window is the
        // topmost window under the pointer. If another window covers the game,
        // Windows may show the native pointer even while Arknights still draws
        // its own cursor into the capture.
        if cursor.uncovered {
            self.observe_native_cursor(cursor.native_visible);
        }

        if !self.self_drawn_enabled {
            return false;
        }

        let cursor_rect = self.cursor_rect(
            cursor.client_x,
            cursor.client_y,
            window.width,
            window.height,
        );
        let cost_bar_rect = cost_bar_rect(roi, window.width, window.height, self.ui_scaler);
        let cost_area_rect = cost_area_rect(window.width, window.height, self.ui_scaler);
        cursor_rect.intersects(cost_bar_rect) || cursor_rect.intersects(cost_area_rect)
    }

    fn observe_native_cursor(&mut self, native_visible: bool) {
        if self.ever_saw_hidden {
            return;
        }

        if native_visible {
            self.self_drawn_enabled = false;
        } else {
            self.ever_saw_hidden = true;
            self.self_drawn_enabled = true;
            log::info!(
                "Arknights PC self-drawn cursor detected; cursor guard stays enabled for this run"
            );
        }
    }

    fn cursor_rect(&self, client_x: i32, client_y: i32, width: u32, height: u32) -> Rect {
        let scale = layout_scale(width, height)
            * roi::ui_edge_scale(self.ui_scaler)
            * cursor_size_factor(self.cursor_size);
        let left = client_x - (CURSOR_HOTSPOT_X_REF * scale).round() as i32;
        let top = client_y - (CURSOR_HOTSPOT_Y_REF * scale).round() as i32;
        let cursor_w = (CURSOR_WIDTH_REF * scale).round().max(1.0) as i32;
        let cursor_h = (CURSOR_HEIGHT_REF * scale).round().max(1.0) as i32;

        Rect {
            left,
            top,
            right: left + cursor_w,
            bottom: top + cursor_h,
        }
    }
}

struct CursorState {
    client_x: i32,
    client_y: i32,
    native_visible: bool,
    uncovered: bool,
}

fn cursor_state_in_client(window: WindowInfo) -> Option<CursorState> {
    if window.width == 0 || window.height == 0 {
        return None;
    }

    let hwnd = HWND(window.hwnd as *mut c_void);

    let mut point = POINT::default();
    unsafe {
        if !GetCursorPos(&mut point).as_bool() {
            return None;
        }
    }

    // Use ScreenToClient so the conversion is always accurate even if the
    // game window has been dragged since the pipeline was started (the
    // cached client_left/client_top in WindowInfo would be stale in that case).
    let screen_x = point.x;
    let screen_y = point.y;
    unsafe {
        if !ScreenToClient(hwnd, &mut point).as_bool() {
            return None;
        }
    }

    let client_x = point.x;
    let client_y = point.y;
    if client_x < 0
        || client_y < 0
        || client_x >= window.width as i32
        || client_y >= window.height as i32
    {
        return None;
    }

    // WindowFromPoint still needs screen coordinates.
    let top_hwnd = unsafe {
        WindowFromPoint(POINT {
            x: screen_x,
            y: screen_y,
        })
    };
    let uncovered = top_hwnd.0 as isize == window.hwnd;
    let native_visible = cursor_info()
        .map(|info| (info.flags & CURSOR_SHOWING) != 0)
        .unwrap_or(true);

    Some(CursorState {
        client_x,
        client_y,
        native_visible,
        uncovered,
    })
}

fn cursor_info() -> Option<CursorInfo> {
    let mut info = CursorInfo {
        cb_size: std::mem::size_of::<CursorInfo>() as u32,
        flags: 0,
        h_cursor: std::ptr::null_mut(),
        pt_screen_pos: POINT::default(),
    };
    unsafe { GetCursorInfo(&mut info).as_bool().then_some(info) }
}

fn cost_bar_rect(roi: Roi, width: u32, height: u32, ui_scaler: f64) -> Rect {
    let scale = layout_scale(width, height) * roi::ui_edge_scale(ui_scaler);
    let half_height = (4.0 * scale).round().max(3.0) as i32;
    let max_x = width as i32;
    let max_y = height as i32;
    Rect {
        left: roi.0.min(roi.1).clamp(0, max_x),
        right: roi.0.max(roi.1).clamp(0, max_x),
        top: (roi.2 - half_height).clamp(0, max_y),
        bottom: (roi.2 + half_height + 1).clamp(0, max_y),
    }
}

fn cost_area_rect(width: u32, height: u32, ui_scaler: f64) -> Rect {
    let scale = layout_scale(width, height) * roi::ui_edge_scale(ui_scaler);
    let max_x = width as i32;
    let max_y = height as i32;
    let pad_y = (2.0 * scale).round().max(1.0) as i32;

    Rect {
        left: (width as f64 - COST_SIGN_LEFT_OFFSET_FROM_RIGHT_REF * scale).floor() as i32,
        right: (width as f64 - COST_SIGN_RIGHT_OFFSET_FROM_RIGHT_REF * scale).ceil() as i32,
        top: (height as f64 - COST_SIGN_TOP_OFFSET_FROM_BOTTOM_REF * scale).floor() as i32 - pad_y,
        bottom: (height as f64 - COST_SIGN_BOTTOM_OFFSET_FROM_BOTTOM_REF * scale).ceil() as i32
            + pad_y,
    }
    .clamped(max_x, max_y)
}

trait ClampRect {
    fn clamped(self, max_x: i32, max_y: i32) -> Self;
}

impl ClampRect for Rect {
    fn clamped(self, max_x: i32, max_y: i32) -> Self {
        Self {
            left: self.left.clamp(0, max_x),
            top: self.top.clamp(0, max_y),
            right: self.right.clamp(0, max_x),
            bottom: self.bottom.clamp(0, max_y),
        }
    }
}

fn layout_scale(width: u32, height: u32) -> f64 {
    if width == 0 || height == 0 {
        return 1.0;
    }
    let aspect = width as f64 / height as f64;
    if aspect >= REF_ASPECT_RATIO {
        height as f64 / REF_HEIGHT
    } else {
        width as f64 / REF_WIDTH
    }
}

fn cursor_size_factor(cursor_size: f64) -> f64 {
    MIN_CURSOR_SIZE_FACTOR + (1.0 - MIN_CURSOR_SIZE_FACTOR) * cursor_size.clamp(0.0, 1.0)
}

fn finite_or_default(value: f64, default: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        default
    }
}
