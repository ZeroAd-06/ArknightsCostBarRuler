use std::process::{Command, Stdio};

use image::ImageFormat;

use crate::analysis::scanner::PixelFormat;
use crate::capture::{CaptureBackend, CapturedFrame};

pub struct AdbController {
    device_id: Option<String>,
    width: u32,
    height: u32,
}

impl AdbController {
    pub fn new(device_id: Option<String>) -> Self {
        Self {
            device_id,
            width: 0,
            height: 0,
        }
    }

    fn run_adb_text(args: &[&str]) -> Result<String, String> {
        let mut command = Command::new("adb");
        configure_hidden_command(&mut command);
        let output = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| format!("failed to run adb: {error}"))?;

        if !output.status.success() {
            return Err(format!(
                "adb {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn run_adb_bytes(args: &[&str]) -> Result<Vec<u8>, String> {
        let mut command = Command::new("adb");
        configure_hidden_command(&mut command);
        let output = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| format!("failed to run adb: {error}"))?;

        if !output.status.success() {
            return Err(format!(
                "adb {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        Ok(output.stdout)
    }

    fn resolve_device_id(&mut self) -> Result<String, String> {
        if let Some(device_id) = self.device_id.as_ref().filter(|id| !id.trim().is_empty()) {
            let device_id = device_id.trim().to_string();
            if looks_like_tcp_serial(&device_id) {
                let _ = Self::run_adb_text(&["connect", &device_id]);
            }
            self.device_id = Some(device_id.clone());
            return Ok(device_id);
        }

        let output = Self::run_adb_text(&["devices"])?;
        let device = parse_first_device(&output)
            .ok_or_else(|| "no available ADB device found".to_string())?;
        self.device_id = Some(device.clone());
        Ok(device)
    }

    fn run_device_text(device_id: &str, args: &[&str]) -> Result<String, String> {
        let mut adb_args = Vec::with_capacity(args.len() + 2);
        adb_args.push("-s");
        adb_args.push(device_id);
        adb_args.extend_from_slice(args);
        Self::run_adb_text(&adb_args)
    }

    fn run_device_bytes(device_id: &str, args: &[&str]) -> Result<Vec<u8>, String> {
        let mut adb_args = Vec::with_capacity(args.len() + 2);
        adb_args.push("-s");
        adb_args.push(device_id);
        adb_args.extend_from_slice(args);
        Self::run_adb_bytes(&adb_args)
    }

    fn resolve_dimensions_from_wm_size(device_id: &str) -> Result<(u32, u32), String> {
        let output = Self::run_device_text(device_id, &["shell", "wm", "size"])?;
        parse_wm_size(&output)
            .ok_or_else(|| format!("failed to parse 'adb shell wm size' output: {output}"))
    }
}

impl CaptureBackend for AdbController {
    fn connect(&mut self) -> Result<(), String> {
        let device_id = self.resolve_device_id()?;
        let state = Self::run_device_text(&device_id, &["get-state"])?;
        if state.trim() != "device" {
            return Err(format!("ADB device '{device_id}' is not ready: {state}"));
        }

        match Self::resolve_dimensions_from_wm_size(&device_id) {
            Ok((width, height)) => {
                self.width = width;
                self.height = height;
            }
            Err(_) => {
                let frame = self.capture_frame()?;
                self.width = frame.width;
                self.height = frame.height;
            }
        }
        Ok(())
    }

    fn capture_frame(&mut self) -> Result<CapturedFrame, String> {
        let device_id = self
            .device_id
            .as_deref()
            .ok_or_else(|| "ADB backend is not connected".to_string())?;
        let png = Self::run_device_bytes(device_id, &["exec-out", "screencap", "-p"])?;
        let image = image::load_from_memory_with_format(&png, ImageFormat::Png)
            .map_err(|error| format!("failed to decode ADB screencap PNG: {error}"))?
            .to_rgba8();

        let width = image.width();
        let height = image.height();
        let data = flip_top_down_rgba_to_bottom_up(image.into_raw(), width, height)?;
        self.width = width;
        self.height = height;

        Ok(CapturedFrame {
            data,
            width,
            height,
            format: PixelFormat::Rgba,
        })
    }

    fn disconnect(&mut self) {
        self.width = 0;
        self.height = 0;
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
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

fn looks_like_tcp_serial(value: &str) -> bool {
    value.contains(':')
        && value
            .rsplit(':')
            .next()
            .is_some_and(|port| port.parse::<u16>().is_ok())
}

fn parse_first_device(output: &str) -> Option<String> {
    output.lines().skip(1).find_map(|line| {
        let mut parts = line.split_whitespace();
        let serial = parts.next()?;
        let state = parts.next()?;
        (state == "device").then(|| serial.to_string())
    })
}

fn parse_wm_size(output: &str) -> Option<(u32, u32)> {
    for line in output.lines() {
        let Some((_, size)) = line.split_once(':') else {
            continue;
        };
        let size = size.trim();
        let Some((width, height)) = size.split_once('x') else {
            continue;
        };
        let width = width.trim().parse::<u32>().ok()?;
        let height = height.trim().parse::<u32>().ok()?;
        if width > 0 && height > 0 {
            return Some((width, height));
        }
    }
    None
}

fn flip_top_down_rgba_to_bottom_up(
    mut data: Vec<u8>,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let stride = width
        .checked_mul(4)
        .ok_or_else(|| "ADB frame stride overflow".to_string())? as usize;
    let expected_len = stride
        .checked_mul(height as usize)
        .ok_or_else(|| "ADB frame size overflow".to_string())?;
    if data.len() != expected_len {
        return Err(format!(
            "ADB decoded frame size mismatch: got {}, expected {expected_len}",
            data.len()
        ));
    }

    for top in 0..(height as usize / 2) {
        let bottom = height as usize - 1 - top;
        let top_offset = top * stride;
        let bottom_offset = bottom * stride;
        for column in 0..stride {
            data.swap(top_offset + column, bottom_offset + column);
        }
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_first_adb_device() {
        let output = "List of devices attached\r\n127.0.0.1:16384\tdevice\r\nfoo\toffline\r\n";
        assert_eq!(
            parse_first_device(output),
            Some("127.0.0.1:16384".to_string())
        );
    }

    #[test]
    fn parses_wm_size_physical_line() {
        assert_eq!(
            parse_wm_size("Physical size: 1280x720\r\nOverride size: 1920x1080"),
            Some((1280, 720))
        );
    }

    #[test]
    fn flips_rgba_rows_for_scanner_layout() {
        let data = vec![
            1, 2, 3, 4, 5, 6, 7, 8, //
            9, 10, 11, 12, 13, 14, 15, 16,
        ];
        let flipped = flip_top_down_rgba_to_bottom_up(data, 2, 2).unwrap();
        assert_eq!(
            flipped,
            vec![
                9, 10, 11, 12, 13, 14, 15, 16, //
                1, 2, 3, 4, 5, 6, 7, 8,
            ]
        );
    }
}
