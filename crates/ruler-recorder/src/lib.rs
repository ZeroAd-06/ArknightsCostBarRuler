use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ruler_core::{BattleState, PixelFormat, TimingDebug};

// ---------------------------------------------------------------------------
// Timestamp helpers
// ---------------------------------------------------------------------------

pub fn timestamp_for_filename() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = d / 86400;
    let time_secs = d % 86400;
    let h = time_secs / 3600;
    let m = (time_secs % 3600) / 60;
    let s = time_secs % 60;
    let (y, mo, day) = days_since_epoch_to_ymd(days as i64);
    format!("{y:04}{mo:02}{day:02}_{h:02}{m:02}{s:02}")
}

fn days_since_epoch_to_ymd(days: i64) -> (i64, u32, u32) {
    let mut y = 1970i64;
    let mut d = days;
    loop {
        let yd = if is_leap(y) { 366 } else { 365 };
        if d < yd {
            break;
        }
        d -= yd;
        y += 1;
    }
    let mon_days: &[u32] = if is_leap(y) {
        &[31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        &[31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 1u32;
    for &md in mon_days {
        if d < md as i64 {
            break;
        }
        d -= md as i64;
        m += 1;
    }
    (y, m, (d + 1) as u32)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

// ---------------------------------------------------------------------------
// Shared pixel helpers
// ---------------------------------------------------------------------------

pub fn pix_fmt_str(fmt: PixelFormat) -> &'static str {
    match fmt {
        PixelFormat::Rgba => "rgba",
        PixelFormat::Bgr => "bgr24",
        PixelFormat::Bgra => "bgra",
    }
}

pub fn bytes_per_pixel(fmt: PixelFormat) -> u32 {
    match fmt {
        PixelFormat::Rgba => 4,
        PixelFormat::Bgr => 3,
        PixelFormat::Bgra => 4,
    }
}

/// Flip rows in-place between top-down and bottom-up order.
pub fn flip_rows(buf: &mut [u8], width: u32, height: u32, bpp: u32) {
    let row_bytes = (width * bpp) as usize;
    for r in 0..(height as usize / 2) {
        let top = r * row_bytes;
        let bot = (height as usize - 1 - r) * row_bytes;
        let (left, right) = buf.split_at_mut(bot);
        left[top..top + row_bytes].swap_with_slice(&mut right[..row_bytes]);
    }
}

// ---------------------------------------------------------------------------
// Calibration helpers
// ---------------------------------------------------------------------------

pub fn calibration_path_from_config(
    config_path: &Path,
    active_profile: Option<&str>,
) -> Option<PathBuf> {
    active_profile.map(|name| {
        config_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("calibration")
            .join(name)
    })
}

pub fn resolve_calibration_path(
    config_path: &Path,
    active_profile: Option<&str>,
    override_path: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(path) = override_path {
        return Ok(path.to_path_buf());
    }

    calibration_path_from_config(config_path, active_profile).ok_or_else(|| {
        "no calibration specified — pass --calibration or set active_calibration_profile in config"
            .to_string()
    })
}

// ---------------------------------------------------------------------------
// Shared CSV writer
// ---------------------------------------------------------------------------

pub struct CsvWriter {
    inner: BufWriter<std::fs::File>,
    frame_count: u64,
}

impl CsvWriter {
    pub fn new(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::File::create(path)?;
        let mut inner = BufWriter::new(file);
        inner.write_all(
            b"frame_index,timestamp_ms,raw_pixel_width,logical_frame,\
              total_frames_in_cycle,cost_is_negative,elapsed_frames,\
              capture_duration_us,phase,battle_state,\
              advanced_frames,accumulator_fp,frames_until_next_cost,\
              match_error_px,boundary_corrected\n",
        )?;
        Ok(Self {
            inner,
            frame_count: 0,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn write_row(
        &mut self,
        timestamp_ms: u128,
        raw_pixel_width: Option<i32>,
        logical_frame: Option<i32>,
        total_frames_in_cycle: i32,
        cost_is_negative: bool,
        elapsed_frames: i32,
        capture_dur_us: u128,
        battle_state: BattleState,
        timing: Option<TimingDebug>,
    ) -> std::io::Result<()> {
        let phase = match (logical_frame, total_frames_in_cycle) {
            (Some(lf), tfc) if tfc > 0 => Some(lf as f64 / tfc as f64),
            _ => None,
        };

        writeln!(
            self.inner,
            "{},{},{},{},{},{},{},{},{},{},{}",
            self.frame_count,
            timestamp_ms,
            raw_pixel_width.map_or(String::new(), |v| v.to_string()),
            logical_frame.map_or(String::new(), |v| v.to_string()),
            total_frames_in_cycle,
            if cost_is_negative { 1 } else { 0 },
            elapsed_frames,
            capture_dur_us,
            phase.map_or(String::new(), |p| format!("{:.6}", p)),
            battle_state.as_str(),
            format_timing_debug(timing),
        )?;
        self.frame_count += 1;
        Ok(())
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Render the five fp24 diagnostic columns (empty when the frame was frozen).
fn format_timing_debug(timing: Option<TimingDebug>) -> String {
    match timing {
        Some(t) => format!(
            "{},{},{},{},{}",
            t.advanced_frames,
            t.accumulator_fp,
            t.frames_until_next_cost,
            t.match_error_px,
            if t.boundary_corrected { 1 } else { 0 },
        ),
        None => ",,,,".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_path_from_config_uses_sibling_directory() {
        let path =
            calibration_path_from_config(Path::new("C:/repo/config.json"), Some("profile_01.json"))
                .unwrap();
        assert_eq!(path, PathBuf::from("C:/repo/calibration/profile_01.json"));
    }

    #[test]
    fn resolve_calibration_path_prefers_override() {
        let path = resolve_calibration_path(
            Path::new("C:/repo/config.json"),
            Some("profile_01.json"),
            Some(Path::new("D:/manual/profile.json")),
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("D:/manual/profile.json"));
    }
}
