//! ruler-recorder — debug helper that records ruler video + analysis data
//!
//! Captures frames at the maximum rate the backend supports, simultaneously:
//!   - Writing a lossless HEVC video file (via ffmpeg pipe)
//!   - Writing a CSV table with per-frame analysis data
//!
//! Usage:
//!   ruler-recorder [-c <config>] [-o <dir>] [-d <secs>]
//!
//!   -c / --config     Config file path (default: config.json)
//!   -o / --output     Output directory (default: .)
//!   -d / --duration   Recording duration in seconds (default: 60)
//!   -h / --help       Print help
//!
//! Press Ctrl+C to stop early.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ruler_core::config::RulerConfig;
use ruler_core::engine::RulerEngine;
use ruler_core::PixelFormat;

// ---------------------------------------------------------------------------
// Global stop flag (set by signal handler, polled by main loop)
// ---------------------------------------------------------------------------

static STOP_NOW: AtomicBool = AtomicBool::new(false);
static ANALYSE_WARNED: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn timestamp_for_filename() -> String {
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

fn pix_fmt_str(fmt: PixelFormat) -> &'static str {
    match fmt {
        PixelFormat::Rgba => "rgba",
        PixelFormat::Bgr => "bgr24",
    }
}

fn bytes_per_pixel(fmt: PixelFormat) -> u32 {
    match fmt {
        PixelFormat::Rgba => 4,
        PixelFormat::Bgr => 3,
    }
}

/// Flip bottom-up rows to top-down (ffmpeg expects top-down).
fn flip_rows(buf: &mut [u8], width: u32, height: u32, bpp: u32) {
    let row_bytes = (width * bpp) as usize;
    for r in 0..(height as usize / 2) {
        let top = r * row_bytes;
        let bot = (height as usize - 1 - r) * row_bytes;
        let (left, right) = buf.split_at_mut(bot);
        left[top..top + row_bytes].swap_with_slice(&mut right[..row_bytes]);
    }
}

// ---------------------------------------------------------------------------
// CSV writer
// ---------------------------------------------------------------------------

struct CsvWriter {
    inner: BufWriter<std::fs::File>,
    frame_count: u64,
    start: Instant,
}

impl CsvWriter {
    fn new(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::File::create(path)?;
        let mut inner = BufWriter::new(file);
        inner.write_all(
            b"frame_index,timestamp_ms,raw_pixel_width,logical_frame,\
              total_frames_in_cycle,cost_is_negative,elapsed_frames,\
              capture_duration_us,phase\n",
        )?;
        Ok(Self {
            inner,
            frame_count: 0,
            start: Instant::now(),
        })
    }

    fn write_row(
        &mut self,
        raw_pixel_width: Option<i32>,
        logical_frame: Option<i32>,
        total_frames_in_cycle: i32,
        cost_is_negative: bool,
        elapsed_frames: i32,
        capture_dur_us: u128,
    ) -> std::io::Result<()> {
        let ts_us = self.start.elapsed().as_micros();
        let phase = match (logical_frame, total_frames_in_cycle) {
            (Some(lf), tfc) if tfc > 0 => Some(lf as f64 / tfc as f64),
            _ => None,
        };

        writeln!(
            self.inner,
            "{},{},{},{},{},{},{},{},{}",
            self.frame_count,
            ts_us / 1000,
            raw_pixel_width.map_or(String::new(), |v| v.to_string()),
            logical_frame.map_or(String::new(), |v| v.to_string()),
            total_frames_in_cycle,
            if cost_is_negative { 1 } else { 0 },
            elapsed_frames,
            capture_dur_us,
            phase.map_or(String::new(), |p| format!("{:.6}", p)),
        )?;
        self.frame_count += 1;
        Ok(())
    }

    fn frame_count(&self) -> u64 {
        self.frame_count
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    // ---- parse args -------------------------------------------------------
    let args: Vec<String> = std::env::args().collect();
    let mut config_path = PathBuf::from("config.json");
    let mut output_dir = PathBuf::from(".");
    let mut duration_secs = 60u64;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" | "-c" => {
                i += 1;
                config_path = PathBuf::from(&args[i]);
            }
            "--output" | "-o" => {
                i += 1;
                output_dir = PathBuf::from(&args[i]);
            }
            "--duration" | "-d" => {
                i += 1;
                duration_secs = args[i]
                    .parse()
                    .expect("--duration must be a number of seconds");
            }
            "--help" | "-h" => {
                eprintln!("Usage: ruler-recorder [OPTIONS]");
                eprintln!("  -c, --config PATH    Config file (default: config.json)");
                eprintln!("  -o, --output DIR     Output dir (default: .)");
                eprintln!("  -d, --duration SECS  Seconds (default: 60)");
                eprintln!("  -h, --help           Print help");
                return;
            }
            _ => {
                eprintln!("Unknown option: {} (use --help)", args[i]);
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let duration = Duration::from_secs(duration_secs);

    // ---- prepare output paths ---------------------------------------------
    std::fs::create_dir_all(&output_dir).unwrap_or_else(|e| {
        eprintln!("FATAL: cannot create output dir '{}': {e}", output_dir.display());
        std::process::exit(1);
    });
    let ts = timestamp_for_filename();
    let csv_path = output_dir.join(format!("recording_{ts}.csv"));
    let video_path = output_dir.join(format!("recording_{ts}.hevc"));

    eprintln!("=== ruler-recorder ===");
    eprintln!("  config     : {}", config_path.display());
    eprintln!("  output dir : {}", output_dir.display());
    eprintln!("  duration   : {duration_secs} s");
    eprintln!("  video      : {}", video_path.display());
    eprintln!("  csv        : {}", csv_path.display());

    // ---- load config ------------------------------------------------------
    let ruler_config = RulerConfig::load_from_path(&config_path).unwrap_or_else(|e| {
        eprintln!("FATAL: failed to load config: {e}");
        std::process::exit(1);
    });
    let capture_config = ruler_config.to_capture_config().unwrap_or_else(|e| {
        eprintln!("FATAL: invalid capture config: {e}");
        std::process::exit(1);
    });

    // ---- locate calibration -----------------------------------------------
    // Calibration files live in `calibration/` subdirectory; the config
    // stores the filename in `active_calibration_profile`.
    let cal_dir = config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("calibration");
    let cal_filename = ruler_config.active_calibration_profile.as_deref();
    let cal_path = cal_filename.map(|name| cal_dir.join(name));

    // ---- init engine ------------------------------------------------------
    let mut engine = RulerEngine::new();
    let (width, height) = engine.connect(capture_config).unwrap_or_else(|e| {
        eprintln!("FATAL: connect failed: {e}");
        std::process::exit(1);
    });
    eprintln!("  connected  : {width}x{height}");

    if let Some(ref p) = cal_path {
        engine.load_calibration(p).unwrap_or_else(|e| {
            eprintln!("FATAL: calibration failed: {e}");
            std::process::exit(1);
        });
        eprintln!("  calibr.    : {}", p.display());
    } else {
        eprintln!("  warning    : no calibration specified in config — analysis fields empty");
    }

    engine.set_roi(width as i32, height as i32);

    // ---- capture first frame to detect pixel format -----------------------
    let first_frame = engine.capture_frame().unwrap_or_else(|e| {
        eprintln!("FATAL: first capture failed: {e}");
        std::process::exit(1);
    });
    let fmt = first_frame.format;
    let bpp = bytes_per_pixel(fmt);
    let frame_bytes = (height as usize) * (width as usize) * (bpp as usize);
    eprintln!("  pixel fmt  : {fmt:?} ({frame_bytes} B/frame)");

    // ---- install Ctrl+C handler -------------------------------------------
    install_ctrlc_handler();

    // ---- spawn ffmpeg -----------------------------------------------------
    let pix_fmt = pix_fmt_str(fmt);
    let mut ffmpeg = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "rawvideo",
            "-pixel_format",
            pix_fmt,
            "-video_size",
            &format!("{width}x{height}"),
            "-framerate",
            "60",
            "-i",
            "pipe:0",
            "-vf",
            "vflip",
            "-c:v",
            "libx265",
            "-crf",
            "0",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv444p",
            "-tag:v",
            "hvc1",
            &video_path.to_string_lossy(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| {
            eprintln!("FATAL: could not spawn ffmpeg: {e}");
            eprintln!("  Make sure ffmpeg is in PATH (>= 5.x).");
            std::process::exit(1);
        });

    let ffmpeg_stdin = ffmpeg.stdin.take().expect("ffmpeg stdin not piped");
    let mut ffmpeg_writer = BufWriter::new(ffmpeg_stdin);

    // ---- open CSV ---------------------------------------------------------
    let mut csv = CsvWriter::new(&csv_path).unwrap_or_else(|e| {
        eprintln!("FATAL: cannot create CSV: {e}");
        std::process::exit(1);
    });

    // ---- write first frame ------------------------------------------------
    let result = engine
        .analyze_captured_frame(&first_frame)
        .unwrap_or_else(|e| {
            eprintln!("WARNING: first frame analysis failed: {e}");
            ruler_core::engine::FrameResult {
                logical_frame: None,
                total_frames_in_cycle: 0,
                raw_pixel_width: None,
                elapsed_frames: 0,
                cost_is_negative: false,
            }
        });
    {
        let mut buf = first_frame.data;
        flip_rows(&mut buf, width, height, bpp);
        ffmpeg_writer.write_all(&buf).expect("ffmpeg write failed");
    }
    csv.write_row(
        result.raw_pixel_width,
        result.logical_frame,
        result.total_frames_in_cycle,
        result.cost_is_negative,
        result.elapsed_frames,
        0,
    )
    .expect("csv write failed");
    let first_frame_count = csv.frame_count();

    // ---- main loop --------------------------------------------------------
    let start_time = Instant::now();
    let deadline = start_time + duration;
    let mut dropped = 0u64;

    while !STOP_NOW.load(Ordering::Relaxed) && Instant::now() < deadline {
        let t0 = Instant::now();

        // 1. Capture
        let frame = match engine.capture_frame() {
            Ok(f) => f,
            Err(e) => {
                eprintln!("\nWARN: capture failed: {e}");
                dropped += 1;
                if dropped > 20 {
                    eprintln!("FATAL: too many consecutive failures");
                    break;
                }
                continue;
            }
        };
        let cap_us = t0.elapsed().as_micros();

        // 2. Analyse (best-effort — missing calibration still records video + partial CSV)
        let result = match engine.analyze_captured_frame(&frame) {
            Ok(r) => r,
            Err(e) => {
                if !ANALYSE_WARNED.swap(true, Ordering::Relaxed) {
                    eprintln!("\nWARN: analysis failed (will retry silently): {e}");
                }
                ruler_core::engine::FrameResult {
                    logical_frame: None,
                    total_frames_in_cycle: 0,
                    raw_pixel_width: None,
                    elapsed_frames: 0,
                    cost_is_negative: false,
                }
            }
        };

        // 3. Pipe to ffmpeg
        let mut buf = frame.data;
        flip_rows(&mut buf, width, height, bpp);
        if let Err(e) = ffmpeg_writer.write_all(&buf) {
            eprintln!("\nFATAL: ffmpeg pipe error: {e}");
            break;
        }

        // 4. Write CSV (always — sync with video frames)
        if let Err(e) = csv.write_row(
            result.raw_pixel_width,
            result.logical_frame,
            result.total_frames_in_cycle,
            result.cost_is_negative,
            result.elapsed_frames,
            cap_us,
        ) {
            eprintln!("\nFATAL: csv write error: {e}");
            break;
        }

        // Progress indicator
        let fc = csv.frame_count();
        if fc % 100 == 0 {
            let elapsed = start_time.elapsed();
            let fps = fc as f64 / elapsed.as_secs_f64();
            eprint!(".");
            if fc % 5000 == 0 {
                eprintln!(" {fps:.0} fps ({fc} frames)");
            }
        }

        // Reset drop counter on success
        dropped = 0;
    }

    // ---- cleanup ----------------------------------------------------------
    ffmpeg_writer.flush().ok();
    drop(ffmpeg_writer);
    let _ = ffmpeg.wait();
    csv.flush().ok();

    let elapsed = start_time.elapsed();
    let total = csv.frame_count() - first_frame_count + 1; // account for first frame
    let fps = if elapsed.as_secs_f64() > 0.0 {
        total as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };

    eprintln!("\n\n=== complete ===");
    eprintln!("  frames     : {total} recorded (+ {dropped} dropped)");
    eprintln!("  duration   : {:.1}s  @ {fps:.0} fps", elapsed.as_secs_f64());
    eprintln!("  video      : {}", video_path.display());
    eprintln!("  csv        : {}", csv_path.display());
}

// ---------------------------------------------------------------------------
// Cross-platform Ctrl+C via static STOP_NOW
// ---------------------------------------------------------------------------

fn install_ctrlc_handler() {
    #[cfg(windows)]
    unsafe extern "system" fn ctrl_handler(_: u32) -> i32 {
        STOP_NOW.store(true, Ordering::SeqCst);
        eprintln!("\n⏹  stopping recording (Ctrl+C)...");
        1 // TRUE = handled
    }

    #[cfg(unix)]
    unsafe extern "C" fn ctrl_handler(_: i32) {
        STOP_NOW.store(true, Ordering::SeqCst);
        unsafe {
            let msg = b"\n\xE2\x8F\xB9  stopping recording (Ctrl+C)...\n";
            libc::write(libc::STDERR_FILENO, msg.as_ptr() as *const _, msg.len());
        }
    }

    #[cfg(windows)]
    {
        // Register via Win32 API (declared inline, no crate needed)
        type HandlerFn = unsafe extern "system" fn(u32) -> i32;
        extern "system" {
            fn SetConsoleCtrlHandler(
                handler: Option<HandlerFn>,
                add: i32,
            ) -> i32;
        }
        unsafe {
            SetConsoleCtrlHandler(Some(ctrl_handler), 1);
        }
    }

    #[cfg(unix)]
    {
        unsafe {
            libc::signal(libc::SIGINT, ctrl_handler as usize);
            libc::signal(libc::SIGTERM, ctrl_handler as usize);
        }
    }
}
