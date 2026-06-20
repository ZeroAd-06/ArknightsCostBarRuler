//! ruler-verifier — offline per-frame analysis of a recorded video file
//!
//! Reads an existing recorded video file, decodes it sequentially through ffmpeg,
//! runs the standard `RulerEngine` analysis on every frame, and writes the
//! results to a CSV file using the same schema as `ruler-recorder`.
//!
//! Usage:
//!   ruler-verifier -i <input.video> [-c <config>] [-o <output.csv>]
//!                   [--fps <value>] [--calibration <path>]

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;

use ruler_core::config::RulerConfig;
use ruler_core::engine::RulerEngine;
use ruler_core::PixelFormat;
use ruler_recorder::{bytes_per_pixel, flip_rows, resolve_calibration_path, CsvWriter};

// ---------------------------------------------------------------------------
// CLI options
// ---------------------------------------------------------------------------

struct Options {
    config_path: PathBuf,
    input_path: PathBuf,
    output_path: PathBuf,
    calibration_path: Option<PathBuf>,
    fps_override: Option<f64>,
    ui_scaler: Option<f64>,
}

fn print_help() {
    eprintln!("Usage: ruler-verifier [OPTIONS]");
    eprintln!("  -c, --config PATH         Config file (default: config.json)");
    eprintln!("  -i, --input PATH          Input recorded video file (required)");
    eprintln!("  -o, --output PATH         Output CSV path (default: <input>_verify.csv)");
    eprintln!("      --fps VALUE           Fallback FPS if stream timestamps are unavailable");
    eprintln!("      --ui-scaler VALUE     Arknights PC UI scaler value (0.0..1.0)");
    eprintln!("      --calibration PATH    Override calibration file path");
    eprintln!("  -h, --help               Print help");
}

fn default_output_path(input_path: &Path) -> PathBuf {
    let stem = input_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "output".to_string());
    input_path.with_file_name(format!("{stem}_verify.csv"))
}

fn parse_args() -> Result<Options, String> {
    let args: Vec<String> = std::env::args().collect();
    let mut config_path = PathBuf::from("config.json");
    let mut input_path: Option<PathBuf> = None;
    let mut output_path: Option<PathBuf> = None;
    let mut calibration_path: Option<PathBuf> = None;
    let mut fps_override: Option<f64> = None;
    let mut ui_scaler: Option<f64> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" | "-c" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "missing value for --config".to_string())?;
                config_path = PathBuf::from(value);
            }
            "--input" | "-i" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "missing value for --input".to_string())?;
                input_path = Some(PathBuf::from(value));
            }
            "--output" | "-o" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "missing value for --output".to_string())?;
                output_path = Some(PathBuf::from(value));
            }
            "--fps" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "missing value for --fps".to_string())?;
                let parsed = value
                    .parse::<f64>()
                    .map_err(|_| "--fps must be a positive number".to_string())?;
                if parsed <= 0.0 {
                    return Err("--fps must be a positive number".to_string());
                }
                fps_override = Some(parsed);
            }
            "--ui-scaler" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "missing value for --ui-scaler".to_string())?;
                let parsed = value
                    .parse::<f64>()
                    .map_err(|_| "--ui-scaler must be a number between 0.0 and 1.0".to_string())?;
                if !(0.0..=1.0).contains(&parsed) {
                    return Err("--ui-scaler must be between 0.0 and 1.0".to_string());
                }
                ui_scaler = Some(parsed);
            }
            "--calibration" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "missing value for --calibration".to_string())?;
                calibration_path = Some(PathBuf::from(value));
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown option: {other} (use --help)")),
        }
        i += 1;
    }

    let input_path = input_path.ok_or_else(|| "--input is required".to_string())?;
    let output_path = output_path.unwrap_or_else(|| default_output_path(&input_path));

    Ok(Options {
        config_path,
        input_path,
        output_path,
        calibration_path,
        fps_override,
        ui_scaler,
    })
}

// ---------------------------------------------------------------------------
// ffprobe / ffmpeg helpers
// ---------------------------------------------------------------------------

enum ReadFrame {
    Frame { timestamp_ms: u128 },
    Eof,
}

struct RawVideoDecoder {
    child: Option<Child>,
    stdout: Option<ChildStdout>,
    frame_size: usize,
    timestamps: Receiver<Result<u128, String>>,
    stderr_thread: Option<JoinHandle<Result<String, String>>>,
    fallback_fps: Option<f64>,
    frames_read: u64,
    warned_timestamp_fallback: bool,
}

fn parse_showinfo_time_base(line: &str) -> Option<(u128, u128)> {
    let (_, rest) = line.split_once("config in time_base:")?;
    let token = rest.trim().split(',').next()?.trim();
    let (num, den) = token.split_once('/')?;
    let num = num.trim().parse::<u128>().ok()?;
    let den = den.trim().parse::<u128>().ok()?;
    if den == 0 {
        return None;
    }
    Some((num, den))
}

fn parse_showinfo_pts(line: &str) -> Option<i128> {
    let (_, rest) = line.split_once(" pts:")?;
    let token = rest.split_whitespace().next()?;
    token.parse::<i128>().ok()
}

fn parse_showinfo_pts_time_ms(line: &str) -> Option<u128> {
    let (_, rest) = line.split_once(" pts_time:")?;
    let token = rest.split_whitespace().next()?;
    let pts_time = token.parse::<f64>().ok()?;
    if !pts_time.is_finite() || pts_time < 0.0 {
        return None;
    }
    Some((pts_time * 1000.0).round() as u128)
}

fn timestamp_ms_from_pts(pts: i128, time_base_num: u128, time_base_den: u128) -> Option<u128> {
    if pts < 0 || time_base_den == 0 {
        return None;
    }

    let pts = pts as u128;
    let numerator = pts.checked_mul(time_base_num)?.checked_mul(1000)?;
    Some((numerator + time_base_den / 2) / time_base_den)
}

fn is_showinfo_frame_line(line: &str) -> bool {
    line.contains(" pts:") && line.contains(" pts_time:")
}

fn parse_showinfo_frame_timestamp_ms(
    line: &str,
    time_base: Option<(u128, u128)>,
) -> Result<u128, String> {
    if let Some((time_base_num, time_base_den)) = time_base {
        if let Some(pts) = parse_showinfo_pts(line) {
            return timestamp_ms_from_pts(pts, time_base_num, time_base_den)
                .ok_or_else(|| format!("failed to convert ffmpeg pts to milliseconds: {line}"));
        }
    }

    parse_showinfo_pts_time_ms(line)
        .ok_or_else(|| format!("failed to parse ffmpeg pts_time: {line}"))
}

fn spawn_timestamp_reader(
    stderr: ChildStderr,
) -> (
    Receiver<Result<u128, String>>,
    JoinHandle<Result<String, String>>,
) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut stderr_log = String::new();
        let mut time_base: Option<(u128, u128)> = None;

        for line_result in BufReader::new(stderr).lines() {
            let line = line_result.map_err(|e| format!("failed to read ffmpeg stderr: {e}"))?;

            stderr_log.push_str(&line);
            stderr_log.push('\n');

            if let Some(parsed) = parse_showinfo_time_base(&line) {
                time_base = Some(parsed);
                continue;
            }

            if !is_showinfo_frame_line(&line) {
                continue;
            }

            let timestamp_ms = match parse_showinfo_frame_timestamp_ms(&line, time_base) {
                Ok(timestamp_ms) => timestamp_ms,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return Ok(stderr_log);
                }
            };

            if tx.send(Ok(timestamp_ms)).is_err() {
                return Ok(stderr_log);
            }
        }

        Ok(stderr_log)
    });

    (rx, handle)
}

impl RawVideoDecoder {
    fn probe_dimensions(input_path: &Path) -> Result<(u32, u32), String> {
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height",
                "-of",
                "csv=s=x:p=0",
                &input_path.to_string_lossy(),
            ])
            .output()
            .map_err(|e| format!("failed to run ffprobe: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr.trim();
            if detail.is_empty() {
                return Err("ffprobe failed to read video dimensions".to_string());
            }
            return Err(format!("ffprobe failed: {detail}"));
        }

        let dims = String::from_utf8_lossy(&output.stdout);
        let dims = dims.trim();
        let mut parts = dims.split('x');
        let width = parts
            .next()
            .ok_or_else(|| format!("unexpected ffprobe output: {dims}"))?
            .parse::<u32>()
            .map_err(|_| format!("invalid ffprobe width: {dims}"))?;
        let height = parts
            .next()
            .ok_or_else(|| format!("unexpected ffprobe output: {dims}"))?
            .parse::<u32>()
            .map_err(|_| format!("invalid ffprobe height: {dims}"))?;

        if width == 0 || height == 0 {
            return Err(format!("invalid video dimensions: {width}x{height}"));
        }

        Ok((width, height))
    }

    fn spawn(
        input_path: &Path,
        width: u32,
        height: u32,
        format: PixelFormat,
        fallback_fps: Option<f64>,
    ) -> Result<Self, String> {
        let frame_size = (width * height * bytes_per_pixel(format)) as usize;
        let pix_fmt = match format {
            PixelFormat::Rgba => "rgba",
            PixelFormat::Bgr => "bgr24",
        };

        let mut child = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-nostats",
                "-v",
                "info",
                "-i",
                &input_path.to_string_lossy(),
                "-vf",
                "showinfo",
                "-fps_mode",
                "passthrough",
                "-f",
                "rawvideo",
                "-pix_fmt",
                pix_fmt,
                "-an",
                "-sn",
                "-dn",
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn ffmpeg decoder: {e}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "ffmpeg stdout not available".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "ffmpeg stderr not available".to_string())?;
        let (timestamps, stderr_thread) = spawn_timestamp_reader(stderr);

        Ok(Self {
            child: Some(child),
            stdout: Some(stdout),
            frame_size,
            timestamps,
            stderr_thread: Some(stderr_thread),
            fallback_fps,
            frames_read: 0,
            warned_timestamp_fallback: false,
        })
    }

    fn read_frame(&mut self, buffer: &mut [u8]) -> Result<ReadFrame, String> {
        if buffer.len() != self.frame_size {
            return Err(format!(
                "decoder buffer size mismatch: expected {}, got {}",
                self.frame_size,
                buffer.len()
            ));
        }

        let stdout = self
            .stdout
            .as_mut()
            .ok_or_else(|| "decoder stdout is not available".to_string())?;

        let mut filled = 0;
        while filled < buffer.len() {
            match stdout.read(&mut buffer[filled..]) {
                Ok(0) if filled == 0 => return Ok(ReadFrame::Eof),
                Ok(0) => {
                    return Err(format!(
                        "decoder ended mid-frame (read {} / {} bytes)",
                        filled,
                        buffer.len()
                    ))
                }
                Ok(n) => filled += n,
                Err(e) => return Err(format!("failed to read decoded frame: {e}")),
            }
        }

        let timestamp_ms = match self.timestamps.recv() {
            Ok(Ok(timestamp_ms)) => timestamp_ms,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                let fps = self.fallback_fps.ok_or_else(|| {
                    "decoder produced a frame without timestamp metadata".to_string()
                })?;
                if !self.warned_timestamp_fallback {
                    eprintln!(
                        "WARNING: ffmpeg timestamp stream ended early; falling back to {:.3} fps",
                        fps
                    );
                    self.warned_timestamp_fallback = true;
                }
                ((self.frames_read as f64) * 1000.0 / fps).round() as u128
            }
        };

        self.frames_read += 1;
        Ok(ReadFrame::Frame { timestamp_ms })
    }

    fn finish(&mut self) -> Result<(), String> {
        self.stdout.take();

        let status = if let Some(mut child) = self.child.take() {
            Some(
                child
                    .wait()
                    .map_err(|e| format!("failed to wait for ffmpeg: {e}"))?,
            )
        } else {
            None
        };

        let stderr_log = if let Some(handle) = self.stderr_thread.take() {
            match handle.join() {
                Ok(Ok(stderr_log)) => stderr_log,
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err("ffmpeg stderr reader panicked".to_string()),
            }
        } else {
            String::new()
        };

        if let Some(status) = status {
            if !status.success() {
                let detail = stderr_log.trim();
                if detail.is_empty() {
                    return Err("ffmpeg decoder exited with a failure status".to_string());
                }
                return Err(format!("ffmpeg decoder failed: {detail}"));
            }
        }

        Ok(())
    }
}

impl Drop for RawVideoDecoder {
    fn drop(&mut self) {
        self.stdout.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let options = parse_args().unwrap_or_else(|e| {
        eprintln!("FATAL: {e}");
        eprintln!();
        print_help();
        std::process::exit(1);
    });

    if !options.input_path.is_file() {
        eprintln!(
            "FATAL: input video file does not exist: {}",
            options.input_path.display()
        );
        std::process::exit(1);
    }

    if let Some(parent) = options.output_path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|e| {
            eprintln!(
                "FATAL: cannot create output directory '{}': {e}",
                parent.display()
            );
            std::process::exit(1);
        });
    }

    let ruler_config = RulerConfig::load_from_path(&options.config_path).unwrap_or_else(|e| {
        eprintln!("FATAL: failed to load config: {e}");
        std::process::exit(1);
    });

    let calibration_path = resolve_calibration_path(
        &options.config_path,
        ruler_config.active_calibration_profile.as_deref(),
        options.calibration_path.as_deref(),
    )
    .unwrap_or_else(|e| {
        eprintln!("FATAL: {e}");
        std::process::exit(1);
    });

    if !calibration_path.is_file() {
        eprintln!(
            "FATAL: calibration file does not exist: {}",
            calibration_path.display()
        );
        std::process::exit(1);
    }

    let fallback_fps = options
        .fps_override
        .or(ruler_config.replay_fps)
        .unwrap_or(60.0);
    if fallback_fps <= 0.0 {
        eprintln!("FATAL: replay FPS must be positive");
        std::process::exit(1);
    }

    let (width, height) =
        RawVideoDecoder::probe_dimensions(&options.input_path).unwrap_or_else(|e| {
            eprintln!("FATAL: {e}");
            std::process::exit(1);
        });

    let mut decoder = RawVideoDecoder::spawn(
        &options.input_path,
        width,
        height,
        PixelFormat::Bgr,
        Some(fallback_fps),
    )
    .unwrap_or_else(|e| {
        eprintln!("FATAL: {e}");
        std::process::exit(1);
    });

    let mut engine = RulerEngine::new();
    engine.set_ui_scaler(
        options
            .ui_scaler
            .unwrap_or_else(|| ruler_config.effective_ui_scaler()),
    );
    engine
        .load_calibration(&calibration_path)
        .unwrap_or_else(|e| {
            eprintln!("FATAL: failed to load calibration: {e}");
            std::process::exit(1);
        });
    engine.set_roi(width as i32, height as i32);

    let mut csv = CsvWriter::new(&options.output_path).unwrap_or_else(|e| {
        eprintln!(
            "FATAL: cannot create CSV '{}': {e}",
            options.output_path.display()
        );
        std::process::exit(1);
    });

    let frame_size = (width * height * bytes_per_pixel(PixelFormat::Bgr)) as usize;
    let mut frame_buf = vec![0u8; frame_size];

    eprintln!("=== ruler-verifier ===");
    eprintln!("  config      : {}", options.config_path.display());
    eprintln!("  input       : {}", options.input_path.display());
    eprintln!("  calibration : {}", calibration_path.display());
    eprintln!("  output      : {}", options.output_path.display());
    eprintln!("  dimensions  : {width}x{height}");
    eprintln!("  ui scaler   : {:.3}", engine.ui_scaler());
    eprintln!("  timestamps  : ffmpeg/showinfo PTS");
    eprintln!("  fps fallback: {fallback_fps}");

    loop {
        match decoder.read_frame(&mut frame_buf) {
            Ok(ReadFrame::Frame { timestamp_ms }) => {
                flip_rows(
                    &mut frame_buf,
                    width,
                    height,
                    bytes_per_pixel(PixelFormat::Bgr),
                );

                let result = engine
                    .analyze_raw_buffer(&frame_buf, width, height, PixelFormat::Bgr)
                    .unwrap_or_else(|e| {
                        eprintln!("FATAL: analysis failed at frame {}: {e}", csv.frame_count());
                        std::process::exit(1);
                    });

                csv.write_row(
                    timestamp_ms,
                    result.raw_pixel_width,
                    result.logical_frame,
                    result.total_frames_in_cycle,
                    result.cost_is_negative,
                    result.elapsed_frames,
                    0,
                    result.battle_state,
                )
                .unwrap_or_else(|e| {
                    eprintln!("FATAL: csv write failed: {e}");
                    std::process::exit(1);
                });

                if csv.frame_count() % 1000 == 0 {
                    eprintln!("  processed   : {} frames", csv.frame_count());
                }
            }
            Ok(ReadFrame::Eof) => break,
            Err(e) => {
                eprintln!("FATAL: {e}");
                std::process::exit(1);
            }
        }
    }

    decoder.finish().unwrap_or_else(|e| {
        eprintln!("FATAL: {e}");
        std::process::exit(1);
    });
    csv.flush().ok();

    eprintln!("\n=== complete ===");
    eprintln!("  frames      : {}", csv.frame_count());
    eprintln!("  output      : {}", options.output_path.display());
}
