//! ruler-verifier — offline per-frame analysis of a recorded HEVC file
//!
//! Reads an existing `.hevc`, decodes it sequentially through ffmpeg, runs the
//! standard `RulerEngine` analysis on every frame, and writes the results to a
//! CSV file using the same schema as `ruler-recorder`.
//!
//! Usage:
//!   ruler-verifier -i <input.hevc> [-c <config>] [-o <output.csv>]
//!                   [--fps <value>] [--calibration <path>]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

use ruler_core::config::RulerConfig;
use ruler_core::engine::RulerEngine;
use ruler_core::PixelFormat;
use ruler_recorder::{
    bytes_per_pixel, flip_rows, resolve_calibration_path, CsvWriter,
};

// ---------------------------------------------------------------------------
// CLI options
// ---------------------------------------------------------------------------

struct Options {
    config_path: PathBuf,
    input_path: PathBuf,
    output_path: PathBuf,
    calibration_path: Option<PathBuf>,
    fps_override: Option<f64>,
}

fn print_help() {
    eprintln!("Usage: ruler-verifier [OPTIONS]");
    eprintln!("  -c, --config PATH         Config file (default: config.json)");
    eprintln!("  -i, --input PATH          Input HEVC file (required)");
    eprintln!("  -o, --output PATH         Output CSV path (default: <input>_verify.csv)");
    eprintln!("      --fps VALUE           Override replay FPS from config");
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
    })
}

// ---------------------------------------------------------------------------
// ffprobe / ffmpeg helpers
// ---------------------------------------------------------------------------

enum ReadFrame {
    Frame,
    Eof,
}

struct RawVideoDecoder {
    child: Option<Child>,
    stdout: Option<ChildStdout>,
    frame_size: usize,
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
    ) -> Result<Self, String> {
        let frame_size = (width * height * bytes_per_pixel(format)) as usize;
        let pix_fmt = match format {
            PixelFormat::Rgba => "rgba",
            PixelFormat::Bgr => "bgr24",
        };

        let mut child = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-i",
                &input_path.to_string_lossy(),
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

        Ok(Self {
            child: Some(child),
            stdout: Some(stdout),
            frame_size,
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

        Ok(ReadFrame::Frame)
    }

    fn finish(&mut self) -> Result<(), String> {
        self.stdout.take();
        if let Some(child) = self.child.take() {
            let output = child
                .wait_with_output()
                .map_err(|e| format!("failed to wait for ffmpeg: {e}"))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let detail = stderr.trim();
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
            "FATAL: input HEVC file does not exist: {}",
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

    let fps = options
        .fps_override
        .or(ruler_config.replay_fps)
        .unwrap_or(60.0);
    if fps <= 0.0 {
        eprintln!("FATAL: replay FPS must be positive");
        std::process::exit(1);
    }

    let (width, height) = RawVideoDecoder::probe_dimensions(&options.input_path).unwrap_or_else(|e| {
        eprintln!("FATAL: {e}");
        std::process::exit(1);
    });

    let mut decoder = RawVideoDecoder::spawn(&options.input_path, width, height, PixelFormat::Bgr)
        .unwrap_or_else(|e| {
            eprintln!("FATAL: {e}");
            std::process::exit(1);
        });

    let mut engine = RulerEngine::new();
    engine.load_calibration(&calibration_path).unwrap_or_else(|e| {
        eprintln!("FATAL: failed to load calibration: {e}");
        std::process::exit(1);
    });
    engine.set_roi(width as i32, height as i32);

    let mut csv = CsvWriter::new(&options.output_path).unwrap_or_else(|e| {
        eprintln!("FATAL: cannot create CSV '{}': {e}", options.output_path.display());
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
    eprintln!("  fps         : {fps}");

    loop {
        match decoder.read_frame(&mut frame_buf) {
            Ok(ReadFrame::Frame) => {
                flip_rows(&mut frame_buf, width, height, bytes_per_pixel(PixelFormat::Bgr));

                let result = engine
                    .analyze_raw_buffer(&frame_buf, width, height, PixelFormat::Bgr)
                    .unwrap_or_else(|e| {
                        eprintln!(
                            "FATAL: analysis failed at frame {}: {e}",
                            csv.frame_count()
                        );
                        std::process::exit(1);
                    });

                let frame_index = csv.frame_count();
                let timestamp_ms = ((frame_index as f64) * 1000.0 / fps).round() as u128;
                csv.write_row(
                    timestamp_ms,
                    result.raw_pixel_width,
                    result.logical_frame,
                    result.total_frames_in_cycle,
                    result.cost_is_negative,
                    result.elapsed_frames,
                    0,
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
