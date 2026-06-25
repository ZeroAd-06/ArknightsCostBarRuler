//! Shared Slint software-rendering + Win32 layered-window plumbing.
//!
//! Both the first-run config wizard ([`crate::config_wizard`]) and the live HUD
//! overlay ([`crate::overlay`]) render Slint components with the software
//! renderer into per-pixel-alpha layered windows. They run sequentially on the
//! same (main) thread — the wizard during `RulerApp::build`, the overlay during
//! `RulerApp::run` — but `slint::platform::set_platform` may only be called once
//! per process. [`ensure_platform`] makes that initialization idempotent so both
//! can claim freshly-minted windows from the same factory slot.

use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    iter,
    rc::Rc,
    time::Instant,
};

use slint::{
    platform::{
        software_renderer::{
            MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, TargetPixel,
        },
        Platform, WindowAdapter, WindowEvent,
    },
    LogicalPosition, PlatformError, SharedString,
};
use windows::Win32::{
    Foundation::{COLORREF, HWND, LPARAM, POINT, SIZE},
    Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, GetDC, ReleaseDC, SelectObject,
        AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
        DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    },
    UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA},
};

/// Shared slot the platform writes each freshly-created window into, so the
/// caller can claim it right after instantiating a component (single-threaded
/// UI thread, so this is race-free).
pub(crate) type WindowSlot = Rc<RefCell<Option<Rc<MinimalSoftwareWindow>>>>;

/// Premultiplied BGRA pixel, the exact layout `UpdateLayeredWindow` expects for
/// a per-pixel-alpha layered window (32bpp top-down DIB, AC_SRC_ALPHA).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct PreBgra {
    pub b: u8,
    pub g: u8,
    pub r: u8,
    pub a: u8,
}

impl TargetPixel for PreBgra {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let inv = (u8::MAX - color.alpha) as u16;
        self.r = (self.r as u16 * inv / 255) as u8 + color.red;
        self.g = (self.g as u16 * inv / 255) as u8 + color.green;
        self.b = (self.b as u16 * inv / 255) as u8 + color.blue;
        self.a = (self.a as u16 * inv / 255) as u8 + color.alpha;
    }

    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        Self {
            b: blue,
            g: green,
            r: red,
            a: 255,
        }
    }

    // Uncovered / transparent areas of the (transparent) Slint window.
    fn background() -> Self {
        Self {
            b: 0,
            g: 0,
            r: 0,
            a: 0,
        }
    }
}

struct RulerPlatform {
    /// Each `create_window_adapter` call mints a fresh window and parks it here
    /// so the caller can claim it (HUD on startup; menu/wizard popups later).
    slot: WindowSlot,
    start: Instant,
}

impl Platform for RulerPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        // NewBuffer = full repaint each frame. Avoids partial-repaint residue
        // (stale glyph fragments) on a per-pixel-alpha layered window.
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        *self.slot.borrow_mut() = Some(window.clone());
        Ok(window)
    }

    fn duration_since_start(&self) -> core::time::Duration {
        self.start.elapsed()
    }
}

thread_local! {
    /// The process-wide factory slot, created the first time [`ensure_platform`]
    /// runs. `Rc` is not `Send`, but the wizard and overlay both run on the main
    /// thread, so a thread-local is sufficient and avoids `unsafe` statics.
    static PLATFORM_SLOT: RefCell<Option<WindowSlot>> = const { RefCell::new(None) };
}

/// Install the Slint platform + bundled fonts exactly once, returning the shared
/// window factory slot. Subsequent calls return a clone of the same slot without
/// touching the (already-set) global platform.
pub(crate) fn ensure_platform() -> WindowSlot {
    PLATFORM_SLOT.with(|cell| {
        if let Some(slot) = cell.borrow().as_ref() {
            return Rc::clone(slot);
        }
        let slot: WindowSlot = Rc::new(RefCell::new(None));
        let result = slint::platform::set_platform(Box::new(RulerPlatform {
            slot: Rc::clone(&slot),
            start: Instant::now(),
        }));
        if let Err(error) = result {
            // Only happens if some other path already set a platform; the fonts
            // still need registering and the caller still needs a slot, so log
            // and proceed with our (unused-by-that-platform) slot.
            log::warn!("set_platform reported an existing platform: {error:?}");
        }
        crate::fonts::register_bundled_fonts();
        *cell.borrow_mut() = Some(Rc::clone(&slot));
        slot
    })
}

/// Create a top-down 32bpp DIB section + memory DC for the software framebuffer.
/// Returns `(mem_dc, dib, bits)` where `bits` points at `width * height` pixels.
pub(crate) unsafe fn create_dib(width: i32, height: i32) -> Option<(HDC, HBITMAP, *mut PreBgra)> {
    let screen = GetDC(HWND::default());
    let mem_dc = CreateCompatibleDC(screen);
    let _ = ReleaseDC(HWND::default(), screen);
    if mem_dc.0.is_null() {
        return None;
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // top-down to match Slint's row order
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let Ok(dib) = CreateDIBSection(mem_dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
        let _ = DeleteDC(mem_dc);
        return None;
    };
    if dib.0.is_null() || bits.is_null() {
        let _ = DeleteDC(mem_dc);
        return None;
    }
    SelectObject(mem_dc, HGDIOBJ(dib.0));
    Some((mem_dc, dib, bits.cast::<PreBgra>()))
}

/// Blit the premultiplied-alpha framebuffer to a layered window.
pub(crate) unsafe fn present_layered(hwnd: HWND, mem_dc: HDC, width: i32, height: i32) {
    let screen = GetDC(HWND::default());
    let size = SIZE {
        cx: width,
        cy: height,
    };
    let src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let _ = UpdateLayeredWindow(
        hwnd,
        screen,
        None,
        Some(&size as *const SIZE),
        mem_dc,
        Some(&src as *const POINT),
        COLORREF(0),
        Some(&blend as *const BLENDFUNCTION),
        ULW_ALPHA,
    );
    let _ = ReleaseDC(HWND::default(), screen);
}

/// Logical pointer position from an `LPARAM`-packed client coordinate.
pub(crate) fn logical_pos(lparam: LPARAM, scale: f32) -> LogicalPosition {
    let x = (lparam.0 as u32 & 0xffff) as i16 as f32;
    let y = ((lparam.0 as u32 >> 16) & 0xffff) as i16 as f32;
    LogicalPosition::new(x / scale, y / scale)
}

/// Raw signed client coordinate from an `LPARAM`.
pub(crate) fn client_xy(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam.0 as u32 & 0xffff) as i16 as i32;
    let y = ((lparam.0 as u32 >> 16) & 0xffff) as i16 as i32;
    (x, y)
}

/// NUL-terminated UTF-16 for Win32 wide-string APIs.
pub(crate) fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(iter::once(0)).collect()
}

/// Dispatch a press + release for `text` (a typed character or a `Key` glyph)
/// to whichever Slint text field currently holds focus (the inline-rename /
/// manual-parameter fields). Shared by the overlay menu and the config wizard.
pub(crate) fn dispatch_key(window: &Rc<MinimalSoftwareWindow>, text: SharedString) {
    let _ = window
        .window()
        .try_dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    let _ = window
        .window()
        .try_dispatch_event(WindowEvent::KeyReleased { text });
}

/// Decode one WM_CHAR UTF-16 code unit into text, buffering the high half of a
/// surrogate pair across calls in `pending_high`. Returns `None` for control
/// characters and for the (stashed) high surrogate.
pub(crate) fn decode_wm_char(pending_high: &Cell<u16>, unit: u16) -> Option<SharedString> {
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
