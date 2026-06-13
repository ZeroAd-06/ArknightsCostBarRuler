use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use libloading::Library;

use crate::analysis::scanner::PixelFormat;
use crate::capture::{CaptureBackend, CapturedFrame};

// DLL function signatures matching external_renderer_ipc.h
type NemuConnectFn = unsafe extern "C" fn(path: *const u16, instance_index: i32) -> i32;
type NemuDisconnectFn = unsafe extern "C" fn(handle: i32);
type NemuCaptureDisplayFn = unsafe extern "C" fn(
    handle: i32,
    display_id: u32,
    buffer_size: i32,
    width: *mut i32,
    height: *mut i32,
    pixels: *mut u8,
) -> i32;
type NemuGetDisplayIdFn =
    unsafe extern "C" fn(handle: i32, package_name: *const u8, app_index: i32) -> i32;

/// Cached DLL function pointers — resolved once at connect time, never again.
struct CachedSymbols {
    connect: unsafe extern "C" fn(path: *const u16, instance_index: i32) -> i32,
    disconnect: unsafe extern "C" fn(handle: i32),
    capture_display: unsafe extern "C" fn(
        handle: i32,
        display_id: u32,
        buffer_size: i32,
        width: *mut i32,
        height: *mut i32,
        pixels: *mut u8,
    ) -> i32,
    get_display_id:
        unsafe extern "C" fn(handle: i32, package_name: *const u8, app_index: i32) -> i32,
}

pub struct MuMuController {
    /// Loaded DLL — kept alive for the entire session.
    _dll: Library,
    /// Cached function pointers — no per-frame dlsym.
    symbols: CachedSymbols,
    /// Connection handle returned by nemu_connect.
    handle: i32,
    /// Display ID for the target app (cached after first resolution).
    display_id: i32,
    /// Frame dimensions.
    width: u32,
    height: u32,
    /// Reusable capture buffer — written into by nemu_capture_display each frame.
    buffer: Vec<u8>,
    /// Install path used for connect.
    install_path: String,
    /// Instance index.
    instance_index: u32,
}

impl MuMuController {
    pub fn new(install_path: String, instance_index: u32) -> Self {
        // We need dummy values before connect; use a sentinel.
        // In practice, connect() must be called before any capture.
        Self {
            _dll: unsafe {
                Library::new("kernel32.dll").unwrap_or_else(|_| panic!("fallback lib"))
            },
            symbols: CachedSymbols {
                connect: unsafe_fn_stub,
                disconnect: unsafe_fn_stub_void,
                capture_display: unsafe_fn_stub_capture,
                get_display_id: unsafe_fn_stub_get_id,
            },
            handle: 0,
            display_id: -1,
            width: 0,
            height: 0,
            buffer: Vec::new(),
            install_path,
            instance_index,
        }
    }

    fn find_dll(install_path: &str) -> Result<(PathBuf, PathBuf), String> {
        let initial = PathBuf::from(install_path);
        let mut bases = vec![initial.clone()];
        if let Some(parent) = initial.parent() {
            if parent != Path::new("") && parent != initial {
                bases.push(parent.to_path_buf());
            }
        }

        let relative_paths = [
            PathBuf::from("nx_device/12.0/shell/sdk/external_renderer_ipc.dll"),
            PathBuf::from("nx_main/sdk/external_renderer_ipc.dll"),
            PathBuf::from("shell/sdk/external_renderer_ipc.dll"),
        ];

        for base in bases {
            for rel in &relative_paths {
                let candidate = base.join(rel);
                if candidate.exists() {
                    return Ok((candidate, base));
                }
            }
        }

        Err("Could not locate external_renderer_ipc.dll under the MuMu install path".to_string())
    }

    /// Resolve all DLL symbols once, caching the raw function pointers.
    /// This is the MAA pattern: load_library → get_function → cache, then call cached ptr directly.
    unsafe fn resolve_symbols(dll: &Library) -> Result<CachedSymbols, String> {
        let connect: libloading::Symbol<'_, NemuConnectFn> = dll
            .get(b"nemu_connect\0")
            .map_err(|e| format!("Failed to load nemu_connect: {e}"))?;
        let disconnect: libloading::Symbol<'_, NemuDisconnectFn> = dll
            .get(b"nemu_disconnect\0")
            .map_err(|e| format!("Failed to load nemu_disconnect: {e}"))?;
        let capture_display: libloading::Symbol<'_, NemuCaptureDisplayFn> = dll
            .get(b"nemu_capture_display\0")
            .map_err(|e| format!("Failed to load nemu_capture_display: {e}"))?;
        let get_display_id: libloading::Symbol<'_, NemuGetDisplayIdFn> = dll
            .get(b"nemu_get_display_id\0")
            .map_err(|e| format!("Failed to load nemu_get_display_id: {e}"))?;

        // Convert Symbol wrappers to raw fn pointers — lifetime-free, no per-call dlsym.
        Ok(CachedSymbols {
            connect: *connect,
            disconnect: *disconnect,
            capture_display: *capture_display,
            get_display_id: *get_display_id,
        })
    }

    const PACKAGE_NAMES: &'static [&'static str] = &[
        "com.hypergryph.arknights",
        "com.hypergryph.arknights.bilibili",
        "tw.txwy.and.arknights",
        "com.YoStarEN.Arknights",
        "com.YoStarJP.Arknights",
        "com.YoStarKR.Arknights",
    ];
}

fn mumu_ipc_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn ipc_guard() -> MutexGuard<'static, ()> {
    mumu_ipc_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

// Dummy stubs for pre-connect initialization (never called in practice).
// Must be extern "C" to match the CachedSymbols fn pointer types.
unsafe extern "C" fn unsafe_fn_stub(_: *const u16, _: i32) -> i32 {
    0
}
unsafe extern "C" fn unsafe_fn_stub_void(_: i32) {}
unsafe extern "C" fn unsafe_fn_stub_capture(
    _: i32,
    _: u32,
    _: i32,
    _: *mut i32,
    _: *mut i32,
    _: *mut u8,
) -> i32 {
    1
}
unsafe extern "C" fn unsafe_fn_stub_get_id(_: i32, _: *const u8, _: i32) -> i32 {
    -1
}

impl CaptureBackend for MuMuController {
    fn connect(&mut self) -> Result<(), String> {
        let (dll_path, resolved_root) = Self::find_dll(&self.install_path)?;
        self.install_path = resolved_root.to_string_lossy().into_owned();
        let instance_index = i32::try_from(self.instance_index)
            .map_err(|_| format!("MuMu instance index {} is too large", self.instance_index))?;

        let dll = unsafe { Library::new(&dll_path) }
            .map_err(|e| format!("Failed to load MuMu DLL '{}': {e}", dll_path.display()))?;

        let symbols = unsafe { Self::resolve_symbols(&dll)? };

        let root_wide = wide_null(&self.install_path);

        let _ipc = ipc_guard();
        let handle = unsafe { (symbols.connect)(root_wide.as_ptr(), instance_index) };
        if handle == 0 {
            return Err(format!(
                "Failed to connect to MuMu instance {}",
                self.instance_index
            ));
        }
        self.handle = handle;

        // Cache display_id — try each Arknights package name
        let mut display_id: i32 = -1;
        for &package_name in Self::PACKAGE_NAMES {
            let pkg_cstr = CString::new(package_name)
                .map_err(|_| format!("Invalid package name: {package_name}"))?;
            let id = unsafe { (symbols.get_display_id)(handle, pkg_cstr.as_ptr() as *const u8, 0) };
            if id >= 0 {
                display_id = id;
                break;
            }
        }
        if display_id < 0 {
            display_id = 0;
        }
        self.display_id = display_id;

        // Query dimensions (buffer_size=0 call per official API)
        let mut width = 0i32;
        let mut height = 0i32;
        let ret = unsafe {
            (symbols.capture_display)(
                handle,
                display_id as u32,
                0,
                &mut width,
                &mut height,
                std::ptr::null_mut(),
            )
        };
        if ret != 0 {
            // Attempt disconnect before returning error
            unsafe { (symbols.disconnect)(handle) };
            self.handle = 0;
            return Err(format!("Failed to query MuMu display dimensions: {ret}"));
        }
        if width <= 0 || height <= 0 {
            unsafe { (symbols.disconnect)(handle) };
            self.handle = 0;
            return Err(format!(
                "MuMu returned invalid dimensions: {width}x{height}"
            ));
        }

        self.width = width as u32;
        self.height = height as u32;
        let buffer_size =
            self.width
                .checked_mul(self.height)
                .and_then(|px| px.checked_mul(4))
                .ok_or_else(|| "MuMu buffer size overflow".to_string())? as usize;
        self.buffer.resize(buffer_size, 0);

        // Store the loaded DLL and cached symbols
        self._dll = dll;
        self.symbols = symbols;

        Ok(())
    }

    fn capture_frame(&mut self) -> Result<CapturedFrame, String> {
        if self.handle == 0 {
            return Err("MuMu backend is not connected".to_string());
        }
        if self.buffer.is_empty() {
            return Err("MuMu capture buffer is not initialized".to_string());
        }

        // Call cached function pointer directly — zero dlsym overhead.
        let mut width = i32::try_from(self.width)
            .map_err(|_| format!("MuMu frame width {} is too large", self.width))?;
        let mut height = i32::try_from(self.height)
            .map_err(|_| format!("MuMu frame height {} is too large", self.height))?;
        let display_id = u32::try_from(self.display_id)
            .map_err(|_| "MuMu display id is not initialized".to_string())?;
        let buffer_size = i32::try_from(self.buffer.len())
            .map_err(|_| "MuMu capture buffer is too large".to_string())?;
        let _ipc = ipc_guard();
        let ret = unsafe {
            (self.symbols.capture_display)(
                self.handle,
                display_id,
                buffer_size,
                &mut width,
                &mut height,
                self.buffer.as_mut_ptr(),
            )
        };
        if ret != 0 {
            return Err(format!("MuMu frame capture failed: {ret}"));
        }
        if width <= 0 || height <= 0 {
            return Err(format!(
                "MuMu returned invalid frame dimensions: {width}x{height}"
            ));
        }

        self.width = width as u32;
        self.height = height as u32;

        // Resize if dimensions changed (rare)
        let frame_size =
            self.width
                .checked_mul(self.height)
                .and_then(|px| px.checked_mul(4))
                .ok_or_else(|| "MuMu frame size overflow".to_string())? as usize;
        if self.buffer.len() != frame_size {
            self.buffer.resize(frame_size, 0);
        }

        // Clone the buffer into the frame result.
        // This is necessary because CapturedFrame owns its data and
        // the caller may hold it while we overwrite self.buffer next frame.
        // MAA avoids this by doing cv::Mat view + cv::cvtColor copy,
        // but our scanner works on raw RGBA bytes so we need owned data.
        Ok(CapturedFrame {
            data: self.buffer.clone(),
            width: self.width,
            height: self.height,
            format: PixelFormat::Rgba,
        })
    }

    fn disconnect(&mut self) {
        if self.handle != 0 {
            let _ipc = ipc_guard();
            unsafe { (self.symbols.disconnect)(self.handle) };
            self.handle = 0;
        }
        self.display_id = -1;
        self.width = 0;
        self.height = 0;
        self.buffer.clear();
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl Drop for MuMuController {
    fn drop(&mut self) {
        self.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_dll_returns_error_for_invalid_path() {
        let result = MuMuController::find_dll("C:\\nonexistent\\path");
        assert!(result.is_err());
    }

    #[test]
    fn wide_null_appends_single_terminator() {
        let encoded = wide_null("D:\\MuMu");

        assert_eq!(encoded.last(), Some(&0));
        assert_eq!(encoded.iter().filter(|&&unit| unit == 0).count(), 1);
        assert_eq!(
            String::from_utf16(&encoded[..encoded.len() - 1]).unwrap(),
            "D:\\MuMu"
        );
    }
}
