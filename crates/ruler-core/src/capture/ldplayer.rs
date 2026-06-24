use std::mem;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::ptr;

use libloading::{Library, Symbol};

use super::adb_resolver::adb_command;
use super::android_settings::AndroidInputOverlayGuard;
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

type CreateScreenShotInstance =
    unsafe extern "C" fn(instance_index: u32, pid: u32) -> *mut LDPlayerObject;

pub struct LDPlayerController {
    pub dll: Option<Library>,
    pub handle: *mut LDPlayerObject,
    pub width: u32,
    pub height: u32,
    pub buffer: Vec<u8>,
    pub spare_buffer: Vec<u8>,
    pub install_path: String,
    pub instance_index: u32,
    pub device_id: Option<String>,
    input_overlay_guard: Option<AndroidInputOverlayGuard>,
}

unsafe impl Send for LDPlayerController {}

impl LDPlayerController {
    pub fn new(install_path: String, instance_index: u32, device_id: Option<String>) -> Self {
        Self {
            dll: None,
            handle: ptr::null_mut(),
            width: 0,
            height: 0,
            buffer: Vec::new(),
            spare_buffer: Vec::new(),
            install_path,
            instance_index,
            device_id,
            input_overlay_guard: None,
        }
    }

    fn run_command(&self, program: &str, args: &[&str]) -> Result<String, String> {
        log::trace!("LDPlayer command: {} {}", program, args.join(" "));
        let mut command = if program.eq_ignore_ascii_case("adb") {
            adb_command()?
        } else {
            let mut command = std::process::Command::new(program);
            configure_hidden_command(&mut command);
            command
        };
        let output = command
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

    /// Resolve the player's PID *and* its configured display dimensions from a
    /// single `dnconsole list2` call.
    ///
    /// `ldopengl64.dll`'s `cap()` returns a bare pointer to the instance's
    /// frame buffer with no size metadata, and that buffer is always sized to
    /// the instance's *configured* resolution — it does **not** follow device
    /// rotation. (LD's own `dnopengl/main.cpp` demo feeds `cap()` straight into
    /// a BMP using `player.width`/`player.height` from this same `list2` row,
    /// and MAA does likewise via `wm size` + `width=max,height=min`.) So the
    /// configured `width,height` fields of `dnconsole list2` are the source of
    /// truth, *not* `dumpsys window displays`' rotation-following `cur=`.
    ///
    /// `list2` rows are comma-separated per LD's `%u,name,topWnd,bndWnd,sysboot,
    /// playerpid,vboxpid,width,height,dpi` format. We pull `playerpid` (field 6)
    /// and `width`/`height` (fields 8/9) from the row matching our instance.
    fn resolve_pid_and_dimensions(&mut self) -> Result<u32, String> {
        let dnconsole = PathBuf::from(&self.install_path).join("dnconsole.exe");
        if !dnconsole.exists() {
            return Err(format!(
                "dnconsole.exe not found at '{}'",
                dnconsole.display()
            ));
        }

        let program = dnconsole.to_string_lossy().into_owned();
        let output = self.run_command(&program, &["list2"])?;
        match parse_list2_row(&output, self.instance_index) {
            Some(InstanceInfo { pid, width, height }) => {
                log::info!(
                    "LDPlayer dimensions from dnconsole list2: {}x{}",
                    width,
                    height
                );
                self.width = width;
                self.height = height;
                Ok(pid)
            }
            None => Err(format!(
                "Unable to find running LDPlayer instance {} in dnconsole list2 output",
                self.instance_index
            )),
        }
    }

    unsafe fn create_instance_symbol(
        &self,
    ) -> Result<Symbol<'_, CreateScreenShotInstance>, String> {
        let dll = self
            .dll
            .as_ref()
            .ok_or_else(|| "LDPlayer DLL not loaded".to_string())?;
        dll.get::<CreateScreenShotInstance>(b"CreateScreenShotInstance\0")
            .map_err(|e| format!("Failed to load CreateScreenShotInstance: {e}"))
    }
}

fn configure_hidden_command(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

impl CaptureBackend for LDPlayerController {
    fn connect(&mut self) -> Result<(), String> {
        self.input_overlay_guard = None;
        log::info!(
            "LDPlayer connect: install_path='{}', instance_index={}, device_id={:?}",
            self.install_path,
            self.instance_index,
            self.device_id
        );
        let pid = self.resolve_pid_and_dimensions()?;
        let device_id = self
            .device_id
            .as_deref()
            .ok_or_else(|| "LDPlayer ADB device id was not resolved".to_string())?;
        self.input_overlay_guard = Some(AndroidInputOverlayGuard::disable_for_device(device_id)?);

        let dll_path = PathBuf::from(&self.install_path).join("ldopengl64.dll");
        if !dll_path.exists() {
            return Err(format!(
                "ldopengl64.dll not found at '{}'",
                dll_path.display()
            ));
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
        let buffer_size =
            self.width
                .checked_mul(self.height)
                .and_then(|px| px.checked_mul(3))
                .ok_or_else(|| "LDPlayer buffer size overflow".to_string())? as usize;
        self.buffer.resize(buffer_size, 0);
        self.spare_buffer.resize(buffer_size, 0);
        log::info!(
            "LDPlayer connected: dimensions={}x{}",
            self.width,
            self.height
        );
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

        let buffer_size =
            self.width
                .checked_mul(self.height)
                .and_then(|px| px.checked_mul(3))
                .ok_or_else(|| "LDPlayer buffer size overflow".to_string())? as usize;
        if self.buffer.len() != buffer_size {
            self.buffer.resize(buffer_size, 0);
        }
        if self.spare_buffer.len() != buffer_size {
            self.spare_buffer.resize(buffer_size, 0);
        }
        unsafe {
            ptr::copy_nonoverlapping(data_ptr, self.buffer.as_mut_ptr(), buffer_size);
        }

        std::mem::swap(&mut self.buffer, &mut self.spare_buffer);
        let frame_data = std::mem::take(&mut self.spare_buffer);

        // `ldopengl64.dll`'s `cap()` returns the GL frame buffer as 3-byte BGR
        // (matching LD's own `dnopengl/main.cpp` demo, which reads `cap()` with
        // `GL_BGR_EXT` and writes it straight into a BMP, and MAA's `CV_8UC3`).
        // The buffer is bottom-up (raw GL Y axis); our scanner already indexes
        // rows bottom-up (`buffer_row = height-1-y`), so no flip is needed.
        Ok(CapturedFrame {
            data: frame_data,
            width: self.width,
            height: self.height,
            format: PixelFormat::Bgr,
        })
    }

    fn disconnect(&mut self) {
        log::info!(
            "LDPlayer disconnect: instance_index={}",
            self.instance_index
        );
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
        self.buffer.clear();
        self.spare_buffer.clear();
        self.input_overlay_guard = None;
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

/// PID + configured display dimensions parsed from one `dnconsole list2` row.
struct InstanceInfo {
    pid: u32,
    width: u32,
    height: u32,
}

/// Parse the row for `instance_index` out of `dnconsole list2` output.
///
/// LD's `list2` rows are comma-separated as
/// `index,name,topWnd,bndWnd,sysboot,playerpid,vboxpid,width,height,dpi`
/// (see LD's own `dnopengl/main.cpp` `parselist2` sscanf format). We pull the
/// player PID (field 6) and the configured width/height (fields 8/9), which is
/// the resolution `ldopengl64.dll`'s `cap()` buffer is sized to. Returns `None`
/// if the instance row is missing or malformed.
fn parse_list2_row(list2_output: &str, instance_index: u32) -> Option<InstanceInfo> {
    let target = instance_index.to_string();
    for line in list2_output.lines() {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 10 || parts[0].trim() != target {
            continue;
        }
        let pid = parts[5].trim().parse::<u32>().ok()?;
        let width = parts[7].trim().parse::<u32>().ok()?;
        let height = parts[8].trim().parse::<u32>().ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        return Some(InstanceInfo { pid, width, height });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pid_and_dimensions_from_list2_row() {
        // Real `dnconsole list2` output from LDPlayer14 instance 0
        // (index,name,topWnd,bndWnd,sysboot,playerpid,vboxpid,width,height,dpi).
        let list2 = "0,雷电模拟器,67573056,23924780,1,60572,40988,1920,1080,280";
        let info = parse_list2_row(list2, 0).expect("instance 0 should parse");
        assert_eq!(info.pid, 60572);
        assert_eq!(info.width, 1920);
        assert_eq!(info.height, 1080);
    }

    #[test]
    fn parses_correct_row_when_multiple_instances_listed() {
        let list2 = "0,雷电模拟器,67573056,23924780,1,60572,40988,1920,1080,280\n\
                     1,雷电模拟器-1,123,456,1,70111,40112,1280,720,280";
        let info = parse_list2_row(list2, 1).expect("instance 1 should parse");
        assert_eq!(info.pid, 70111);
        assert_eq!(info.width, 1280);
        assert_eq!(info.height, 720);
    }

    #[test]
    fn parse_list2_returns_none_for_missing_instance() {
        let list2 = "0,雷电模拟器,67573056,23924780,1,60572,40988,1920,1080,280";
        assert!(parse_list2_row(list2, 9).is_none());
    }

    #[test]
    fn parse_list2_returns_none_for_empty() {
        assert!(parse_list2_row("", 0).is_none());
    }
}
