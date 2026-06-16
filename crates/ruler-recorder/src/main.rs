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
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ruler_core::config::RulerConfig;
use ruler_core::engine::{FrameResult, RulerEngine};
use ruler_recorder::{
    bytes_per_pixel, calibration_path_from_config, flip_rows, pix_fmt_str,
    timestamp_for_filename, CsvWriter,
};

// ---------------------------------------------------------------------------
// Global stop flag (set by signal handler, polled by main loop)
// ---------------------------------------------------------------------------

static STOP_NOW: AtomicBool = AtomicBool::new(false);
static ANALYSE_WARNED: AtomicBool = AtomicBool::new(false);

fn empty_frame_result() -> FrameResult {
    FrameResult {
        logical_frame: None,
        total_frames_in_cycle: 0,
        raw_pixel_width: None,
        elapsed_frames: 0,
        cost_is_negative: false,
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
    let cal_path = calibration_path_from_config(
        &config_path,
        ruler_config.active_calibration_profile.as_deref(),
    );

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
    let csv_start = Instant::now();

    // ---- write first frame ------------------------------------------------
    let result = engine.analyze_captured_frame(&first_frame).unwrap_or_else(|e| {
        eprintln!("WARNING: first frame analysis failed: {e}");
        empty_frame_result()
    });
    {
        let mut buf = first_frame.data;
        flip_rows(&mut buf, width, height, bpp);
        ffmpeg_writer.write_all(&buf).expect("ffmpeg write failed");
    }
    csv.write_row(
        csv_start.elapsed().as_millis(),
        result.raw_pixel_width,
        result.logical_frame,
        result.total_frames_in_cycle,
        result.cost_is_negative,
        result.elapsed_frames,
        0,
    )
    .expect("csv write failed");

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
                empty_frame_result()
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
            csv_start.elapsed().as_millis(),
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
    let total = csv.frame_count();
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
            fn SetConsoleCtrlHandler(handler: Option<HandlerFn>, add: i32) -> i32;
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
