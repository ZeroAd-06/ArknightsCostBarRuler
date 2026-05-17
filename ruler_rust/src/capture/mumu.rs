use std::ffi::CString;
use std::path::{Path, PathBuf};

use libloading::{Library, Symbol};

use crate::analysis::scanner::PixelFormat;
use crate::capture::{CaptureBackend, CapturedFrame};

type NemuConnect = unsafe extern "C" fn(path: *const i8, instance_index: i32) -> i32;
type NemuDisconnect = unsafe extern "C" fn(handle: i32);
type NemuCaptureDisplay = unsafe extern "C" fn(
    handle: i32,
    display_id: i32,
    buffer_size: i32,
    width: *mut i32,
    height: *mut i32,
    buffer: *mut u8,
) -> i32;
type NemuGetDisplayId = unsafe extern "C" fn(handle: i32, package_name: *const u8, unknown: i32) -> i32;

pub struct MuMuController {
    pub dll: Option<Library>,
    pub handle: i32,
    pub display_id: i32,
    pub width: u32,
    pub height: u32,
    pub buffer: Vec<u8>,
    pub install_path: String,
    pub instance_index: u32,
    pub package_names: Vec<String>,
}

impl MuMuController {
    pub fn new(install_path: String, instance_index: u32) -> Self {
        Self {
            dll: None,
            handle: 0,
            display_id: -1,
            width: 0,
            height: 0,
            buffer: Vec::new(),
            install_path,
            instance_index,
            package_names: vec![
                "com.hypergryph.arknights".to_string(),
                "com.hypergryph.arknights.bilibili".to_string(),
                "tw.txwy.and.arknights".to_string(),
                "com.YoStarEN.Arknights".to_string(),
                "com.YoStarJP.Arknights".to_string(),
                "com.YoStarKR.Arknights".to_string(),
            ],
        }
    }

    fn find_dll(&self) -> Result<(PathBuf, PathBuf), String> {
        let initial = PathBuf::from(&self.install_path);
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

    unsafe fn load_symbol<T>(&self, name: &[u8]) -> Result<Symbol<'_, T>, String> {
        let dll = self
            .dll
            .as_ref()
            .ok_or_else(|| "MuMu DLL not loaded".to_string())?;
        dll.get::<T>(name)
            .map_err(|e| format!("Failed to load symbol {}: {e}", String::from_utf8_lossy(name)))
    }
}

impl CaptureBackend for MuMuController {
    fn connect(&mut self) -> Result<(), String> {
        let (dll_path, resolved_root) = self.find_dll()?;
        self.install_path = resolved_root.to_string_lossy().into_owned();

        let dll = unsafe { Library::new(&dll_path) }
            .map_err(|e| format!("Failed to load MuMu DLL '{}': {e}", dll_path.display()))?;
        self.dll = Some(dll);

        let root_cstr = CString::new(self.install_path.clone())
            .map_err(|_| "MuMu install path contains interior NUL byte".to_string())?;

        let handle = unsafe {
            let nemu_connect: Symbol<'_, NemuConnect> = self.load_symbol(b"nemu_connect\0")?;
            nemu_connect(root_cstr.as_ptr(), self.instance_index as i32)
        };
        if handle == 0 {
            return Err(format!(
                "Failed to connect to MuMu instance {}",
                self.instance_index
            ));
        }
        self.handle = handle;

        let mut display_id = -1;
        for package_name in &self.package_names {
            let package = CString::new(package_name.as_str())
                .map_err(|_| format!("Invalid package name: {package_name}"))?;
            let current = unsafe {
                let get_display_id: Symbol<'_, NemuGetDisplayId> =
                    self.load_symbol(b"nemu_get_display_id\0")?;
                get_display_id(self.handle, package.as_ptr() as *const u8, 0)
            };
            if current >= 0 {
                display_id = current;
                break;
            }
        }
        if display_id < 0 {
            display_id = 0;
        }
        self.display_id = display_id;

        let mut width = 0i32;
        let mut height = 0i32;
        let ret = unsafe {
            let capture_display: Symbol<'_, NemuCaptureDisplay> =
                self.load_symbol(b"nemu_capture_display\0")?;
            capture_display(
                self.handle,
                self.display_id,
                0,
                &mut width,
                &mut height,
                std::ptr::null_mut(),
            )
        };
        if ret != 0 {
            self.disconnect();
            return Err(format!("Failed to query MuMu display dimensions: {ret}"));
        }
        if width <= 0 || height <= 0 {
            self.disconnect();
            return Err(format!("MuMu returned invalid dimensions: {width}x{height}"));
        }

        self.width = width as u32;
        self.height = height as u32;
        let buffer_size = self
            .width
            .checked_mul(self.height)
            .and_then(|px| px.checked_mul(4))
            .ok_or_else(|| "MuMu buffer size overflow".to_string())? as usize;
        self.buffer = vec![0u8; buffer_size];

        Ok(())
    }

    fn capture_frame(&mut self) -> Result<CapturedFrame, String> {
        if self.handle == 0 || self.dll.is_none() {
            return Err("MuMu backend is not connected".to_string());
        }
        if self.buffer.is_empty() {
            return Err("MuMu capture buffer is not initialized".to_string());
        }

        let mut width = self.width as i32;
        let mut height = self.height as i32;
        let ret = unsafe {
            let capture_display: Symbol<'_, NemuCaptureDisplay> =
                self.load_symbol(b"nemu_capture_display\0")?;
            capture_display(
                self.handle,
                self.display_id,
                self.buffer.len() as i32,
                &mut width,
                &mut height,
                self.buffer.as_mut_ptr(),
            )
        };
        if ret != 0 {
            return Err(format!("MuMu frame capture failed: {ret}"));
        }
        if width <= 0 || height <= 0 {
            return Err(format!("MuMu returned invalid frame dimensions: {width}x{height}"));
        }

        self.width = width as u32;
        self.height = height as u32;
        Ok(CapturedFrame {
            data: self.buffer.clone(),
            width: self.width,
            height: self.height,
            format: PixelFormat::Rgba,
        })
    }

    fn disconnect(&mut self) {
        if self.handle != 0 {
            if self.dll.is_some() {
                unsafe {
                    if let Ok(disconnect) = self.load_symbol::<NemuDisconnect>(b"nemu_disconnect\0") {
                        disconnect(self.handle);
                    }
                }
            }
            self.handle = 0;
        }
        self.display_id = -1;
        self.width = 0;
        self.height = 0;
        self.buffer.clear();
        self.dll = None;
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
