use std::mem;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::ptr;

use libloading::{Library, Symbol};

use crate::analysis::scanner::PixelFormat;
use crate::capture::{CaptureBackend, CapturedFrame};

#[repr(C)]
pub struct LDPlayerObject {
    pub vtable: *const LDPlayerVTable,
}

#[repr(C)]
pub struct LDPlayerVTable {
    pub release: unsafe extern "C" fn(this: *mut LDPlayerObject),
    pub cap: unsafe extern "C" fn(this: *mut LDPlayerObject) -> *mut u8,
}

type CreateScreenShotInstance = unsafe extern "C" fn(instance_index: u32, pid: u32) -> *mut LDPlayerObject;

pub struct LDPlayerController {
    pub dll: Option<Library>,
    pub handle: *mut LDPlayerObject,
    pub width: u32,
    pub height: u32,
    pub install_path: String,
    pub instance_index: u32,
    pub device_id: Option<String>,
}

unsafe impl Send for LDPlayerController {}

impl LDPlayerController {
    pub fn new(install_path: String, instance_index: u32, device_id: Option<String>) -> Self {
        Self {
            dll: None,
            handle: ptr::null_mut(),
            width: 0,
            height: 0,
            install_path,
            instance_index,
            device_id,
        }
    }

    fn run_command(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .output()
            .map_err(|e| format!("Failed to run '{program}': {e}"))?;

        if !output.status.success() {
            return Err(format!(
                "Command '{}' failed: {}",
                program,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn resolve_device_id(&mut self) -> Result<String, String> {
        if let Some(device_id) = &self.device_id {
            return Ok(device_id.clone());
        }

        let output = self.run_command("adb", &["devices"])?;
        let device = output
            .lines()
            .skip(1)
            .find_map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return None;
                }
                let mut parts = trimmed.split_whitespace();
                let id = parts.next()?;
                let status = parts.next()?;
                (status == "device").then(|| id.to_string())
            })
            .ok_or_else(|| "No available ADB device found for LDPlayer resolution query".to_string())?;
        self.device_id = Some(device.clone());
        Ok(device)
    }

    fn resolve_dimensions(&mut self) -> Result<(), String> {
        let device_id = self.resolve_device_id()?;
        let output = self.run_command("adb", &["-s", &device_id, "shell", "wm", "size"])?;
        let size_line = output
            .lines()
            .find(|line| line.contains("Physical size"))
            .ok_or_else(|| format!("Failed to parse 'adb shell wm size' output: {output}"))?;
        let size = size_line
            .split(':')
            .nth(1)
            .ok_or_else(|| format!("Malformed wm size output: {size_line}"))?
            .trim();
        let mut parts = size.split('x');
        let width = parts
            .next()
            .ok_or_else(|| "Missing width in wm size output".to_string())?
            .parse::<u32>()
            .map_err(|e| format!("Invalid width from wm size: {e}"))?;
        let height = parts
            .next()
            .ok_or_else(|| "Missing height in wm size output".to_string())?
            .parse::<u32>()
            .map_err(|e| format!("Invalid height from wm size: {e}"))?;

        self.width = width;
        self.height = height;
        Ok(())
    }

    fn resolve_pid(&self) -> Result<u32, String> {
        let dnconsole = PathBuf::from(&self.install_path).join("dnconsole.exe");
        if !dnconsole.exists() {
            return Err(format!("dnconsole.exe not found at '{}'", dnconsole.display()));
        }

        let program = dnconsole.to_string_lossy().into_owned();
        let output = self.run_command(&program, &["list2"])?;
        for line in output.lines() {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 6 && parts[0].trim() == self.instance_index.to_string() {
                return parts[5]
                    .trim()
                    .parse::<u32>()
                    .map_err(|e| format!("Invalid LDPlayer PID in dnconsole output: {e}"));
            }
        }

        Err(format!(
            "Unable to find running LDPlayer instance {} in dnconsole list2 output",
            self.instance_index
        ))
    }

    unsafe fn create_instance_symbol(&self) -> Result<Symbol<'_, CreateScreenShotInstance>, String> {
        let dll = self
            .dll
            .as_ref()
            .ok_or_else(|| "LDPlayer DLL not loaded".to_string())?;
        dll.get::<CreateScreenShotInstance>(b"CreateScreenShotInstance\0")
            .map_err(|e| format!("Failed to load CreateScreenShotInstance: {e}"))
    }
}

impl CaptureBackend for LDPlayerController {
    fn connect(&mut self) -> Result<(), String> {
        self.resolve_dimensions()?;
        let pid = self.resolve_pid()?;

        let dll_path = PathBuf::from(&self.install_path).join("ldopengl64.dll");
        if !dll_path.exists() {
            return Err(format!("ldopengl64.dll not found at '{}'", dll_path.display()));
        }

        let dll = unsafe { Library::new(&dll_path) }
            .map_err(|e| format!("Failed to load LDPlayer DLL '{}': {e}", dll_path.display()))?;
        self.dll = Some(dll);

        let handle = unsafe {
            let create_instance = self.create_instance_symbol()?;
            create_instance(self.instance_index, pid)
        };
        if handle.is_null() {
            self.disconnect();
            return Err(format!(
                "CreateScreenShotInstance returned null for instance {} pid {}",
                self.instance_index, pid
            ));
        }

        self.handle = handle;
        Ok(())
    }

    fn capture_frame(&mut self) -> Result<CapturedFrame, String> {
        if self.handle.is_null() {
            return Err("LDPlayer backend is not connected".to_string());
        }
        if self.width == 0 || self.height == 0 {
            return Err("LDPlayer dimensions are not initialized".to_string());
        }

        let vtable = unsafe {
            let vtable_ptr = (*self.handle).vtable;
            if vtable_ptr.is_null() {
                return Err("LDPlayer vtable pointer is null".to_string());
            }
            &*vtable_ptr
        };

        let data_ptr = unsafe { (vtable.cap)(self.handle) };
        if data_ptr.is_null() {
            return Err("LDPlayer cap() returned a null pointer".to_string());
        }

        let buffer_size = self
            .width
            .checked_mul(self.height)
            .and_then(|px| px.checked_mul(3))
            .ok_or_else(|| "LDPlayer buffer size overflow".to_string())? as usize;
        let mut data = vec![0u8; buffer_size];
        unsafe {
            ptr::copy_nonoverlapping(data_ptr, data.as_mut_ptr(), buffer_size);
        }

        Ok(CapturedFrame {
            data,
            width: self.width,
            height: self.height,
            format: PixelFormat::Bgr,
        })
    }

    fn disconnect(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                let vtable_ptr = (*self.handle).vtable;
                if !vtable_ptr.is_null() {
                    ((*vtable_ptr).release)(self.handle);
                }
            }
            self.handle = ptr::null_mut();
        }
        self.width = 0;
        self.height = 0;
        let _ = mem::take(&mut self.dll);
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl Drop for LDPlayerController {
    fn drop(&mut self) {
        self.disconnect();
    }
}
