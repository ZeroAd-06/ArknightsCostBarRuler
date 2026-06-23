//! Debug recorder — background capture of video + analysis data for debugging.
//!
//! Controlled exclusively via config.json (no UI).  When `debug_recording_enabled`
//! is true, the worker thread pipes every captured frame through ffmpeg to a
//! session-local lossless HEVC MKV file and/or logs analysis results to a CSV file.
//!
//! Both outputs are written to `{session_dir}/capture.mkv` and
//! `{session_dir}/analysis.csv` respectively.
//!
//! In the three-layer architecture, [`DebugRecorderConsumer`] wraps a
//! pipeline `ConsumerPipe` (InOrder policy) + `DebugRecorder` and runs a
//! dedicated thread that receives every captured frame from Layer 1 and
//! records it. This decouples recording from the analysis layer (which uses
//! SkipToLatest and may drop intermediate frames).

use std::{
    fs,
    io::{BufWriter, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::Arc,
    thread::{self, JoinHandle},
    time::Instant,
};

use ruler_core::{
    capture::CapturedFrame, engine::FrameResult, pipeline::frame::Frame as PipelineFrame,
    PixelFormat,
};

// ---------------------------------------------------------------------------
// Helpers (shared with ruler-recorder)
// ---------------------------------------------------------------------------

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
// CSV writer (private)
// ---------------------------------------------------------------------------

struct CsvWriter {
    inner: BufWriter<fs::File>,
    frame_count: u64,
    start: Instant,
}

impl CsvWriter {
    fn new(path: &Path) -> std::io::Result<Self> {
        let file = fs::File::create(path)?;
        let mut inner = BufWriter::new(file);
        inner.write_all(
            b"frame_index,timestamp_ms,raw_pixel_width,logical_frame,\
              total_frames_in_cycle,cost_is_negative,elapsed_frames,\
              capture_duration_us,phase,battle_state\n",
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
        battle_state: ruler_core::BattleState,
    ) -> std::io::Result<()> {
        let ts_us = self.start.elapsed().as_micros();
        let phase = match (logical_frame, total_frames_in_cycle) {
            (Some(lf), tfc) if tfc > 0 => Some(lf as f64 / tfc as f64),
            _ => None,
        };

        writeln!(
            self.inner,
            "{},{},{},{},{},{},{},{},{},{}",
            self.frame_count,
            ts_us / 1000,
            raw_pixel_width.map_or(String::new(), |v| v.to_string()),
            logical_frame.map_or(String::new(), |v| v.to_string()),
            total_frames_in_cycle,
            if cost_is_negative { 1 } else { 0 },
            elapsed_frames,
            capture_dur_us,
            phase.map_or(String::new(), |p| format!("{:.6}", p)),
            battle_state.as_str(),
        )?;
        self.frame_count += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// FFmpeg child wrapper — ensures proper shutdown on Drop
// ---------------------------------------------------------------------------

use std::process::ChildStdin;

/// Rawvideo stdin packets have no embedded timestamps, so ffmpeg quantizes
/// wallclock capture time to the input stream time base. 60 fps is too coarse
/// once the backend captures faster than ~16.7 ms/frame, which causes repeated
/// PTS values and later frame drops in verifier/replay. Use a 1 kHz nominal
/// input rate so wallclock timestamps are preserved at millisecond precision.
const FFMPEG_WALLCLOCK_INPUT_FPS: &str = "1000";

struct FfmpegPipe {
    /// Option so we can move out and drop stdin *before* waiting on the child.
    stdin: Option<ChildStdin>,
    child: Option<Child>,
}

impl FfmpegPipe {
    fn spawn(pix_fmt: &str, width: u32, height: u32, output_path: &Path) -> Result<Self, String> {
        let mut child = Command::new("ffmpeg")
            .args([
                "-y",
                "-use_wallclock_as_timestamps",
                "1",
                "-f",
                "rawvideo",
                "-pixel_format",
                pix_fmt,
                "-video_size",
                &format!("{width}x{height}"),
                "-framerate",
                FFMPEG_WALLCLOCK_INPUT_FPS,
                "-i",
                "pipe:0",
                "-an",
                "-sn",
                "-dn",
                "-copyts",
                "-start_at_zero",
                "-fps_mode",
                "passthrough",
                "-c:v",
                "libx265",
                "-crf",
                "0",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv444p",
                &output_path.to_string_lossy(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn ffmpeg: {e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "ffmpeg stdin not available".to_string())?;

        Ok(Self {
            stdin: Some(stdin),
            child: Some(child),
        })
    }

    fn write_frame(&mut self, buf: &[u8]) -> std::io::Result<()> {
        match self.stdin {
            Some(ref mut w) => {
                w.write_all(buf)?;
                w.flush()
            }
            None => Ok(()),
        }
    }
}

impl Drop for FfmpegPipe {
    fn drop(&mut self) {
        // 1. Flush + drop stdin → ffmpeg sees EOF
        if let Some(mut w) = self.stdin.take() {
            let _ = w.flush();
            drop(w);
        }
        // 2. Now wait for ffmpeg to finish
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// DebugRecorder — public API
// ---------------------------------------------------------------------------

/// Records video + analysis data for debugging.  Controlled via config.json.
pub struct DebugRecorder {
    ffmpeg: Option<FfmpegPipe>,
    csv: Option<CsvWriter>,
    width: u32,
    height: u32,
    bpp: u32,
}

impl DebugRecorder {
    /// Start recording.  Only spawns ffmpeg/CSV for the flags that are `true`.
    ///
    /// `output_dir` must already exist.
    pub fn start(
        output_dir: &Path,
        record_video: bool,
        record_csv: bool,
        width: u32,
        height: u32,
        fmt: PixelFormat,
    ) -> Result<Self, String> {
        let bpp = bytes_per_pixel(fmt);

        let ffmpeg = if record_video {
            let video_path = output_dir.join("capture.mkv");
            let pix_fmt = pix_fmt_str(fmt);
            log::info!("debug recording: video -> {}", video_path.display());
            Some(FfmpegPipe::spawn(pix_fmt, width, height, &video_path)?)
        } else {
            None
        };

        let csv = if record_csv {
            let csv_path = output_dir.join("analysis.csv");
            log::info!("debug recording: csv -> {}", csv_path.display());
            Some(
                CsvWriter::new(&csv_path)
                    .map_err(|e| format!("cannot create CSV '{}': {e}", csv_path.display()))?,
            )
        } else {
            None
        };

        Ok(Self {
            ffmpeg,
            csv,
            width,
            height,
            bpp,
        })
    }

    /// Record the video payload for one captured frame as early as possible so
    /// ffmpeg's wallclock timestamps track capture timing instead of post-analysis delay.
    pub fn record_video_frame(&mut self, frame: &CapturedFrame) {
        if let Some(ref mut pipe) = self.ffmpeg {
            let mut buf = frame.data.clone();
            flip_rows(&mut buf, self.width, self.height, self.bpp);
            if let Err(e) = pipe.write_frame(&buf) {
                log::error!("debug recording: ffmpeg write error, stopping video: {e}");
                self.ffmpeg = None;
            }
        }
    }

    /// Record the analysis CSV row for one frame.
    pub fn record_analysis_row(&mut self, result: &FrameResult, capture_dur_us: u128) {
        if let Some(ref mut csv) = self.csv {
            if let Err(e) = csv.write_row(
                result.raw_pixel_width,
                result.logical_frame,
                result.total_frames_in_cycle,
                result.cost_is_negative,
                result.elapsed_frames,
                capture_dur_us,
                result.battle_state,
            ) {
                log::error!("debug recording: csv write error, stopping csv: {e}");
                self.csv = None;
            }
        }
    }

    /// Record a pipeline frame (Layer 1 `Frame`). Equivalent to
    /// `record_video_frame` but accepts the pipeline's `Arc<Vec<u8>>`-backed
    /// frame type instead of `CapturedFrame`.
    pub fn record_pipeline_frame(&mut self, frame: &PipelineFrame) {
        if let Some(ref mut pipe) = self.ffmpeg {
            let mut buf = (*frame.data).clone();
            flip_rows(&mut buf, self.width, self.height, self.bpp);
            if let Err(e) = pipe.write_frame(&buf) {
                log::error!("debug recording: ffmpeg write error, stopping video: {e}");
                self.ffmpeg = None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// DebugRecorderConsumer — Layer 1 InOrder consumer that runs on its own thread
// ---------------------------------------------------------------------------

use ruler_core::pipeline::{ConsumerPipe, PipelineError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::path::PathBuf;

/// Configuration for spawning a [`DebugRecorderConsumer`].
pub struct DebugRecorderConfig {
    pub output_dir: PathBuf,
    pub record_video: bool,
    pub record_csv: bool,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

/// A Layer 1 consumer that records every captured frame to video (and
/// optionally CSV). Runs on its own thread; shutting down the pipeline or
/// dropping this struct stops the thread.
///
/// Note: in the three-layer architecture, the analysis CSV records only the
/// frames that the L2 analyzer actually processed (SkipToLatest may skip
/// frames under load). The video recording, by contrast, captures every
/// frame because this consumer uses InOrder policy.
pub struct DebugRecorderConsumer {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl DebugRecorderConsumer {
    /// Spawn the consumer thread. `pipe` must be an InOrder consumer
    /// connected to the pipeline.
    pub fn spawn(mut pipe: ConsumerPipe, config: DebugRecorderConfig) -> Result<Self, String> {
        let recorder = DebugRecorder::start(
            &config.output_dir,
            config.record_video,
            config.record_csv,
            config.width,
            config.height,
            config.format,
        )?;

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);

        let handle = thread::Builder::new()
            .name("ruler-debug-recorder".to_string())
            .spawn(move || {
                let mut recorder = recorder;
                log::info!(
                    "debug recorder consumer started: video={}, csv={}",
                    config.record_video, config.record_csv
                );
                while running_clone.load(Ordering::Relaxed) {
                    match pipe.recv_frame() {
                        Ok(frame) => {
                            recorder.record_pipeline_frame(&frame);
                            if let Err(err) = pipe.ack(frame.id) {
                                log::debug!("debug recorder: ack failed: {err}");
                                break;
                            }
                        }
                        Err(err) => {
                            log::debug!("debug recorder: recv_frame failed: {err}");
                            break;
                        }
                    }
                }
                log::info!("debug recorder consumer exiting");
            })
            .map_err(|e| format!("failed to spawn debug recorder thread: {e}"))?;

        Ok(Self {
            running,
            handle: Some(handle),
        })
    }
}

impl Drop for DebugRecorderConsumer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
