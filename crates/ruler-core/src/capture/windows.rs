use std::ffi::c_void;

use crate::analysis::scanner::PixelFormat;
use crate::capture::{CaptureBackend, CapturedFrame};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, SRCCOPY,
};

const PW_CLIENTONLY: u32 = 0x0000_0001;
const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumWindows(
        lp_enum_func: Option<unsafe extern "system" fn(HWND, LPARAM) -> BOOL>,
        lparam: LPARAM,
    ) -> BOOL;
    fn FindWindowW(lp_class_name: *const u16, lp_window_name: *const u16) -> HWND;
    fn GetClassNameW(hwnd: HWND, lp_class_name: *mut u16, n_max_count: i32) -> i32;
    fn GetClientRect(hwnd: HWND, lp_rect: *mut RECT) -> BOOL;
    fn GetWindowTextLengthW(hwnd: HWND) -> i32;
    fn GetWindowTextW(hwnd: HWND, lp_string: *mut u16, n_max_count: i32) -> i32;
    fn IsIconic(hwnd: HWND) -> BOOL;
    fn IsWindow(hwnd: HWND) -> BOOL;
    fn IsWindowVisible(hwnd: HWND) -> BOOL;
    fn PrintWindow(hwnd: HWND, hdc_blt: HDC, n_flags: u32) -> BOOL;
    fn ClientToScreen(hwnd: HWND, lp_point: *mut POINT) -> BOOL;
    fn GetDC(hwnd: HWND) -> HDC;
    fn GetWindowDC(hwnd: HWND) -> HDC;
    fn ReleaseDC(hwnd: HWND, hdc: HDC) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn BitBlt(
        hdc: HDC,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        hdc_src: HDC,
        x1: i32,
        y1: i32,
        rop: u32,
    ) -> BOOL;
    fn CreateCompatibleBitmap(hdc: HDC, cx: i32, cy: i32) -> HBITMAP;
    fn CreateCompatibleDC(hdc: HDC) -> HDC;
    fn DeleteDC(hdc: HDC) -> BOOL;
    fn DeleteObject(ho: HGDIOBJ) -> BOOL;
    fn GetDIBits(
        hdc: HDC,
        hbm: HBITMAP,
        start: u32,
        c_lines: u32,
        lpv_bits: *mut c_void,
        lpbmi: *mut BITMAPINFO,
        usage: u32,
    ) -> i32;
    fn SelectObject(hdc: HDC, h: HGDIOBJ) -> HGDIOBJ;
}

struct SearchContext {
    title: Option<String>,
    class: Option<String>,
    found: Option<HWND>,
}

unsafe extern "system" fn enum_windows_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let context = unsafe { &mut *(lparam.0 as *mut SearchContext) };

    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return BOOL(1);
    }

    let title_len = unsafe { GetWindowTextLengthW(hwnd) };
    let title = if title_len > 0 {
        let mut buffer = vec![0u16; title_len as usize + 1];
        let read = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
        String::from_utf16_lossy(&buffer[..read as usize])
    } else {
        String::new()
    };

    let mut class_buffer = vec![0u16; 256];
    let class_len = unsafe { GetClassNameW(hwnd, class_buffer.as_mut_ptr(), class_buffer.len() as i32) };
    let class_name = String::from_utf16_lossy(&class_buffer[..class_len.max(0) as usize]);

    let title_match = context
        .title
        .as_ref()
        .map(|expected| title.contains(expected))
        .unwrap_or(true);
    let class_match = context
        .class
        .as_ref()
        .map(|expected| class_name.to_lowercase().contains(&expected.to_lowercase()))
        .unwrap_or(true);

    if title_match && class_match {
        context.found = Some(hwnd);
        BOOL(0)
    } else {
        BOOL(1)
    }
}

pub struct WindowsController {
    pub hwnd: Option<HWND>,
    pub width: u32,
    pub height: u32,
    pub client_left: i32,
    pub client_top: i32,
    pub hdc_window: Option<HDC>,
    pub hdc_mem: Option<HDC>,
    pub bmp: Option<HBITMAP>,
    pub dib_buffer: Vec<u8>,
    pub spare_dib_buffer: Vec<u8>,
    pub packed_buffer: Vec<u8>,
    pub spare_packed_buffer: Vec<u8>,
    pub window_title: Option<String>,
    pub window_class: Option<String>,
}

unsafe impl Send for WindowsController {}

impl WindowsController {
    pub fn new(
        window_handle: Option<isize>,
        window_title: Option<String>,
        window_class: Option<String>,
    ) -> Self {
        Self {
            hwnd: window_handle.map(|value| HWND(value as *mut c_void)),
            width: 0,
            height: 0,
            client_left: 0,
            client_top: 0,
            hdc_window: None,
            hdc_mem: None,
            bmp: None,
            dib_buffer: Vec::new(),
            spare_dib_buffer: Vec::new(),
            packed_buffer: Vec::new(),
            spare_packed_buffer: Vec::new(),
            window_title,
            window_class,
        }
    }

    fn wide_null(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn locate_window(&self) -> Result<HWND, String> {
        if let Some(hwnd) = self.hwnd {
            if unsafe { IsWindow(hwnd) }.as_bool() {
                return Ok(hwnd);
            }
        }

        if let Some(title) = &self.window_title {
            let title_buf = Self::wide_null(title);
            let class_buf = self.window_class.as_deref().map(Self::wide_null);
            let hwnd = unsafe {
                FindWindowW(
                    class_buf
                        .as_ref()
                        .map(|buf| buf.as_ptr())
                        .unwrap_or(std::ptr::null()),
                    title_buf.as_ptr(),
                )
            };
            if !hwnd.0.is_null() {
                return Ok(hwnd);
            }
        }

        let mut context = SearchContext {
            title: self.window_title.clone(),
            class: self.window_class.clone(),
            found: None,
        };
        unsafe {
            let _ = EnumWindows(
                Some(enum_windows_proc),
                LPARAM((&mut context as *mut SearchContext) as isize),
            );
        }
        context
            .found
            .ok_or_else(|| "Could not find a matching target window".to_string())
    }

    fn cleanup_gdi(&mut self) {
        unsafe {
            if let Some(bmp) = self.bmp.take() {
                let _ = DeleteObject(HGDIOBJ(bmp.0));
            }
            if let Some(hdc_mem) = self.hdc_mem.take() {
                let _ = DeleteDC(hdc_mem);
            }
            if let Some(hdc_window) = self.hdc_window.take() {
                let _ = ReleaseDC(
                    self.hwnd.unwrap_or(HWND(std::ptr::null_mut())),
                    hdc_window,
                );
            }
        }
    }
}

impl CaptureBackend for WindowsController {
    fn connect(&mut self) -> Result<(), String> {
        self.cleanup_gdi();

        let hwnd = self.locate_window()?;
        self.hwnd = Some(hwnd);

        if !unsafe { IsWindow(hwnd) }.as_bool() {
            return Err(format!("Invalid target HWND: {:?}", hwnd));
        }

        let mut rect = RECT::default();
        if !unsafe { GetClientRect(hwnd, &mut rect) }.as_bool() {
            return Err("GetClientRect failed".to_string());
        }
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return Err(format!("Invalid target client size: {width}x{height}"));
        }

        let mut client_origin = POINT { x: 0, y: 0 };
        if !unsafe { ClientToScreen(hwnd, &mut client_origin) }.as_bool() {
            return Err("ClientToScreen failed".to_string());
        }

        let hdc_window = unsafe { GetWindowDC(hwnd) };
        if hdc_window.0.is_null() {
            return Err("GetWindowDC returned null".to_string());
        }

        let hdc_mem = unsafe { CreateCompatibleDC(hdc_window) };
        if hdc_mem.0.is_null() {
            unsafe {
                let _ = ReleaseDC(hwnd, hdc_window);
            }
            return Err("CreateCompatibleDC returned null".to_string());
        }

        let bmp = unsafe { CreateCompatibleBitmap(hdc_window, width, height) };
        if bmp.0.is_null() {
            unsafe {
                let _ = DeleteDC(hdc_mem);
                let _ = ReleaseDC(hwnd, hdc_window);
            }
            return Err("CreateCompatibleBitmap returned null".to_string());
        }

        let selected = unsafe { SelectObject(hdc_mem, HGDIOBJ(bmp.0)) };
        if selected.0.is_null() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(bmp.0));
                let _ = DeleteDC(hdc_mem);
                let _ = ReleaseDC(hwnd, hdc_window);
            }
            return Err("SelectObject failed for capture bitmap".to_string());
        }

        self.width = width as u32;
        self.height = height as u32;
        self.client_left = client_origin.x;
        self.client_top = client_origin.y;
        let expected_stride = self.width as usize * 3;
        let row_stride = (expected_stride + 3) & !3;
        let dib_len = row_stride * self.height as usize;
        let packed_len = expected_stride * self.height as usize;
        self.dib_buffer.resize(dib_len, 0);
        self.spare_dib_buffer.resize(dib_len, 0);
        self.packed_buffer.resize(packed_len, 0);
        self.spare_packed_buffer.resize(packed_len, 0);
        self.hdc_window = Some(hdc_window);
        self.hdc_mem = Some(hdc_mem);
        self.bmp = Some(bmp);
        Ok(())
    }

    fn capture_frame(&mut self) -> Result<CapturedFrame, String> {
        let hwnd = self
            .hwnd
            .ok_or_else(|| "Windows backend is not connected".to_string())?;
        let hdc_mem = self
            .hdc_mem
            .ok_or_else(|| "Memory DC is not initialized".to_string())?;
        let hdc_window = self
            .hdc_window
            .ok_or_else(|| "Window DC is not initialized".to_string())?;
        let bmp = self
            .bmp
            .ok_or_else(|| "Capture bitmap is not initialized".to_string())?;
        if self.dib_buffer.is_empty() {
            return Err("Windows frame buffers are not initialized".to_string());
        }

        let width = self.width as i32;
        let height = self.height as i32;
        let mut captured = false;

        unsafe {
            if PrintWindow(hwnd, hdc_mem, PW_CLIENTONLY | PW_RENDERFULLCONTENT).as_bool()
                || PrintWindow(hwnd, hdc_mem, PW_RENDERFULLCONTENT).as_bool()
            {
                captured = true;
            }

            if !captured && BitBlt(hdc_mem, 0, 0, width, height, hdc_window, 0, 0, SRCCOPY.0).as_bool() {
                captured = true;
            }

            if !captured {
                let screen_dc = GetDC(HWND(std::ptr::null_mut()));
                if !screen_dc.0.is_null() {
                    captured = BitBlt(
                        hdc_mem,
                        0,
                        0,
                        width,
                        height,
                        screen_dc,
                        self.client_left,
                        self.client_top,
                        SRCCOPY.0,
                    )
                    .as_bool();
                    let _ = ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);
                }
            }

            if !captured && IsIconic(hwnd).as_bool() {
                return Err("Target window is minimized and capture fallback chain failed".to_string());
            }
            if !captured {
                return Err("All Windows capture methods failed".to_string());
            }

            let expected_stride = self.width as usize * 3;
            let row_stride = (expected_stride + 3) & !3;
            let dib_len = row_stride * self.height as usize;
            let packed_len = expected_stride * self.height as usize;
            if self.dib_buffer.len() != dib_len {
                self.dib_buffer.resize(dib_len, 0);
            }
            if self.spare_dib_buffer.len() != dib_len {
                self.spare_dib_buffer.resize(dib_len, 0);
            }
            if self.packed_buffer.len() != packed_len {
                self.packed_buffer.resize(packed_len, 0);
            }
            if self.spare_packed_buffer.len() != packed_len {
                self.spare_packed_buffer.resize(packed_len, 0);
            }
            let mut bitmap_info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: self.width as i32,
                    biHeight: self.height as i32,
                    biPlanes: 1,
                    biBitCount: 24,
                    biCompression: BI_RGB.0,
                    biSizeImage: self.dib_buffer.len() as u32,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [Default::default(); 1],
            };

            let scan_lines = GetDIBits(
                hdc_mem,
                bmp,
                0,
                self.height,
                self.dib_buffer.as_mut_ptr() as *mut c_void,
                &mut bitmap_info,
                DIB_RGB_COLORS.0 as u32,
            );
            if scan_lines == 0 {
                return Err("GetDIBits failed to extract bitmap pixels".to_string());
            }

            let data = if row_stride == expected_stride {
                std::mem::swap(&mut self.dib_buffer, &mut self.spare_dib_buffer);
                std::mem::take(&mut self.spare_dib_buffer)
            } else {
                for row in 0..self.height as usize {
                    let src = row * row_stride;
                    let dst = row * expected_stride;
                    self.packed_buffer[dst..dst + expected_stride]
                        .copy_from_slice(&self.dib_buffer[src..src + expected_stride]);
                }

                std::mem::swap(&mut self.packed_buffer, &mut self.spare_packed_buffer);
                std::mem::take(&mut self.spare_packed_buffer)
            };

            Ok(CapturedFrame {
                data,
                width: self.width,
                height: self.height,
                format: PixelFormat::Bgr,
            })
        }
    }

    fn disconnect(&mut self) {
        self.cleanup_gdi();
        self.width = 0;
        self.height = 0;
        self.client_left = 0;
        self.client_top = 0;
        self.dib_buffer.clear();
        self.spare_dib_buffer.clear();
        self.packed_buffer.clear();
        self.spare_packed_buffer.clear();
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl Drop for WindowsController {
    fn drop(&mut self) {
        self.disconnect();
    }
}
