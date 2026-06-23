//! Real capture/analyze benchmark for the configured backend.
//!
//! Usage:
//!   cargo run --release -p ruler-core --example bench_real_env -- --frames 600

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use ruler_core::{
    analysis::{
        roi::find_cost_bar_roi,
        scanner::{get_raw_filled_pixel_width, is_cost_negative},
    },
    capture::create_backend,
    config::RulerConfig,
    Analyzer,
};

#[derive(Clone, Debug)]
struct Options {
    config_path: PathBuf,
    calibration_path: Option<PathBuf>,
    frames: usize,
    max_seconds: f64,
    warmup_frames: usize,
}

#[derive(Default)]
struct Timings {
    capture_ns: Vec<u128>,
    bar_scan_ns: Vec<u128>,
    negative_scan_ns: Vec<u128>,
    engine_analyze_ns: Vec<u128>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let Some(options) = parse_args()? else {
        return Ok(());
    };
    let config = RulerConfig::load_from_path(&options.config_path).map_err(|e| e.to_string())?;
    let capture_config = config.to_capture_config().map_err(|e| e.to_string())?;
    let calibration_path = match options.calibration_path.clone() {
        Some(path) => Some(path),
        None => find_default_calibration(
            &options.config_path,
            config.active_calibration_profile.as_deref(),
        )?,
    };

    println!("== real environment benchmark ==");
    println!("config      : {}", options.config_path.display());
    println!("capture     : {}", config.capture_type);
    println!(
        "calibration : {}",
        calibration_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<none>".to_string())
    );
    println!("frames      : {}", options.frames);
    println!("max_seconds : {:.3}", options.max_seconds);
    println!("warmup      : {}", options.warmup_frames);

    let mut backend = create_backend(capture_config)?;
    let connect_started = Instant::now();
    backend.connect()?;
    let connect_elapsed = connect_started.elapsed();

    let first_capture_started = Instant::now();
    let first = backend.capture_frame()?;
    let first_capture_elapsed = first_capture_started.elapsed();
    let roi = find_cost_bar_roi(first.width as i32, first.height as i32);
    println!("connect_ms  : {:.3}", ms(connect_elapsed));
    println!("first_cap_ms: {:.3}", ms(first_capture_elapsed));
    println!("resolution  : {}x{}", first.width, first.height);
    println!("format      : {:?}", first.format);
    println!("cost_bar_roi: ({}, {}, {})", roi.0, roi.1, roi.2);

    let mut engine = Analyzer::new();
    if let Some(path) = &calibration_path {
        engine.load_calibration(path)?;
        engine.set_roi(first.width as i32, first.height as i32);
    }

    for _ in 0..options.warmup_frames {
        let frame = backend.capture_frame()?;
        if calibration_path.is_some() {
            let _ =
                engine.analyze_raw_buffer(&frame.data, frame.width, frame.height, frame.format)?;
        } else {
            let _ = get_raw_filled_pixel_width(
                &frame.data,
                frame.width,
                frame.height,
                frame.format,
                roi,
            );
            let _ = is_cost_negative(&frame.data, frame.width, frame.height, frame.format);
        }
    }

    let mut timings = Timings::default();
    let started = Instant::now();
    let deadline = started + Duration::from_secs_f64(options.max_seconds);
    let mut valid_bar_count = 0usize;
    let mut negative_count = 0usize;
    let mut logical_frame_count = 0usize;
    let mut last_frame = None;
    let mut last_width = None;

    while timings.capture_ns.len() < options.frames && Instant::now() < deadline {
        let capture_started = Instant::now();
        let frame = backend.capture_frame()?;
        timings
            .capture_ns
            .push(capture_started.elapsed().as_nanos());

        let bar_started = Instant::now();
        let raw_width =
            get_raw_filled_pixel_width(&frame.data, frame.width, frame.height, frame.format, roi);
        timings.bar_scan_ns.push(bar_started.elapsed().as_nanos());
        if raw_width.is_some() {
            valid_bar_count += 1;
        }
        last_width = raw_width;

        let negative_started = Instant::now();
        let is_negative = is_cost_negative(&frame.data, frame.width, frame.height, frame.format);
        timings
            .negative_scan_ns
            .push(negative_started.elapsed().as_nanos());
        if is_negative {
            negative_count += 1;
        }

        if calibration_path.is_some() {
            let analyze_started = Instant::now();
            let result =
                engine.analyze_raw_buffer(&frame.data, frame.width, frame.height, frame.format)?;
            timings
                .engine_analyze_ns
                .push(analyze_started.elapsed().as_nanos());
            if result.logical_frame.is_some() {
                logical_frame_count += 1;
            }
            last_frame = result.logical_frame;
        }
    }

    backend.disconnect();

    let elapsed = started.elapsed();
    let frames = timings.capture_ns.len();
    println!();
    println!("== results ==");
    println!("sampled_frames       : {frames}");
    println!("elapsed_ms           : {:.3}", ms(elapsed));
    println!(
        "loop_fps             : {:.1}",
        frames as f64 / elapsed.as_secs_f64().max(1e-9)
    );
    println!("valid_bar_samples    : {valid_bar_count}/{frames}");
    println!("negative_samples     : {negative_count}/{frames}");
    println!("logical_frame_samples: {logical_frame_count}/{frames}");
    println!("last_raw_width/frame : {:?} / {:?}", last_width, last_frame);
    println!();
    print_stats("capture_frame", &timings.capture_ns);
    print_stats("bar_scan_only", &timings.bar_scan_ns);
    print_stats("negative_only", &timings.negative_scan_ns);
    print_stats("engine_analyze", &timings.engine_analyze_ns);

    Ok(())
}

fn print_stats(name: &str, values: &[u128]) {
    if values.is_empty() {
        println!("{name:<16}: <not measured>");
        return;
    }

    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let sum: u128 = sorted.iter().sum();
    let mean = sum as f64 / sorted.len() as f64;
    println!(
        "{name:<16}: mean={:>8.3} ms  p50={:>8.3}  p95={:>8.3}  p99={:>8.3}  max={:>8.3}",
        ns_to_ms(mean),
        ns_to_ms(percentile(&sorted, 50) as f64),
        ns_to_ms(percentile(&sorted, 95) as f64),
        ns_to_ms(percentile(&sorted, 99) as f64),
        ns_to_ms(*sorted.last().unwrap() as f64),
    );
}

fn percentile(sorted: &[u128], percentile: usize) -> u128 {
    let index = ((sorted.len() - 1) * percentile + 50) / 100;
    sorted[index]
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn ns_to_ms(ns: f64) -> f64 {
    ns / 1_000_000.0
}

fn find_default_calibration(
    config_path: &Path,
    active_profile: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let root = config_path
        .parent()
        .ok_or_else(|| format!("config has no parent: {}", config_path.display()))?;
    let calibration_dir = root.join("calibration");
    if let Some(profile) = active_profile {
        let path = calibration_dir.join(profile);
        if path.is_file() {
            return Ok(Some(path));
        }
    }

    let mut candidates = fs::read_dir(&calibration_dir)
        .map_err(|e| format!("failed to read {}: {e}", calibration_dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let is_json = path.extension().and_then(|ext| ext.to_str()) == Some("json");
            if !is_json {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(modified, _)| *modified);
    Ok(candidates.pop().map(|(_, path)| path))
}

fn parse_args() -> Result<Option<Options>, String> {
    let mut options = Options {
        config_path: default_config_path(),
        calibration_path: None,
        frames: 600,
        max_seconds: 10.0,
        warmup_frames: 30,
    };

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                options.config_path = PathBuf::from(require_value(&mut args, "--config")?)
            }
            "--calibration" => {
                options.calibration_path =
                    Some(PathBuf::from(require_value(&mut args, "--calibration")?));
            }
            "--frames" => {
                options.frames = require_value(&mut args, "--frames")?
                    .parse()
                    .map_err(|_| "--frames must be a positive integer".to_string())?;
            }
            "--max-seconds" => {
                options.max_seconds = require_value(&mut args, "--max-seconds")?
                    .parse()
                    .map_err(|_| "--max-seconds must be a number".to_string())?;
            }
            "--warmup" => {
                options.warmup_frames = require_value(&mut args, "--warmup")?
                    .parse()
                    .map_err(|_| "--warmup must be a non-negative integer".to_string())?;
            }
            "-h" | "--help" => {
                print_help();
                return Ok(None);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    if options.frames == 0 {
        return Err("--frames must be greater than zero".to_string());
    }
    if options.max_seconds <= 0.0 {
        return Err("--max-seconds must be greater than zero".to_string());
    }

    Ok(Some(options))
}

fn require_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn default_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("config.json")
}

fn print_help() {
    println!(
        "Real capture/analyze benchmark\n\n\
Usage:\n  \
cargo run --release -p ruler-core --example bench_real_env -- [options]\n\n\
Options:\n  \
--config <path>       config.json path, default workspace config.json\n  \
--calibration <path>  calibration profile, default active profile or newest calibration/*.json\n  \
--frames <n>          measured frames, default 600\n  \
--max-seconds <sec>   stop after this many seconds, default 10\n  \
--warmup <n>          warmup frames before measurement, default 30\n  \
-h, --help            show help"
    );
}
