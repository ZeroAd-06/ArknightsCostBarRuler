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

use ruler_core::{engine::FrameResult, pipeline::frame::Frame as PipelineFrame, PixelFormat};

// ---------------------------------------------------------------------------
// Helpers (shared with ruler-recorder)
// ---------------------------------------------------------------------------

fn pix_fmt_str(fmt: PixelFormat) -> &'static str {
    match fmt {
        PixelFormat::Rgba => "rgba",
        PixelFormat::Bgr => "bgr24",
        PixelFormat::Bgra => "bgra",
    }
}

fn bytes_per_pixel(fmt: PixelFormat) -> u32 {
    match fmt {
        PixelFormat::Rgba => 4,
        PixelFormat::Bgr => 3,
        PixelFormat::Bgra => 4,
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
              capture_duration_us,phase,battle_state,\
              required_fp,speed_fp,accumulator_fp,advanced_frames,\
              frames_since_cycle_start,frames_until_next_cost,match_error_px\n",
        )?;
        Ok(Self {
            inner,
            frame_count: 0,
            start: Instant::now(),
        })
    }

    fn write_row(&mut self, result: &FrameResult, capture_dur_us: u128) -> std::io::Result<()> {
        let ts_us = self.start.elapsed().as_micros();
        let phase = match (result.logical_frame, result.total_frames_in_cycle) {
            (Some(lf), tfc) if tfc > 0 => Some(lf as f64 / tfc as f64),
            _ => None,
        };
        let debug = result.timing_debug;

        writeln!(
            self.inner,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            self.frame_count,
            ts_us / 1000,
            result
                .raw_pixel_width
                .map_or(String::new(), |v| v.to_string()),
            result
                .logical_frame
                .map_or(String::new(), |v| v.to_string()),
            result.total_frames_in_cycle,
            if result.cost_is_negative { 1 } else { 0 },
            result.elapsed_frames,
            capture_dur_us,
            phase.map_or(String::new(), |p| format!("{:.6}", p)),
            result.battle_state.as_str(),
            debug.map_or(String::new(), |v| v.required_fp.to_string()),
            debug.map_or(String::new(), |v| v.speed_fp.to_string()),
            debug.map_or(String::new(), |v| v.accumulator_fp.to_string()),
            debug.map_or(String::new(), |v| v.advanced_frames.to_string()),
            debug.map_or(String::new(), |v| v.frames_since_cycle_start.to_string()),
            debug.map_or(String::new(), |v| v.frames_until_next_cost.to_string()),
            debug.map_or(String::new(), |v| v.match_error_px.to_string()),
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

    /// Record the analysis CSV row for one frame.
    pub fn record_analysis_row(&mut self, result: &FrameResult, capture_dur_us: u128) {
        if let Some(ref mut csv) = self.csv {
            if let Err(e) = csv.write_row(result, capture_dur_us) {
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

use ruler_core::pipeline::ConsumerPipe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DebugRecordingPlan {
    pub analysis_csv: bool,
    pub raw_video: bool,
    pub raw_csv: bool,
}

impl DebugRecordingPlan {
    #[must_use]
    pub const fn from_flags(enabled: bool, record_video: bool, record_csv: bool) -> Self {
        if enabled {
            Self {
                analysis_csv: record_csv,
                raw_video: record_video,
                raw_csv: false,
            }
        } else {
            Self {
                analysis_csv: false,
                raw_video: false,
                raw_csv: false,
            }
        }
    }

    #[must_use]
    pub const fn has_output(self) -> bool {
        self.analysis_csv || self.raw_video || self.raw_csv
    }
}

/// Configuration for spawning a [`DebugRecorderConsumer`].
pub struct DebugRecorderConfig {
    pub output_dir: PathBuf,
    pub record_video: bool,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

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
            false,
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
                    "debug recorder consumer started: video={}",
                    config.record_video
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

#[cfg(test)]
mod tests {
    use super::*;
    use ruler_core::BattleState;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn debug_recording_routes_csv_to_analyzer_and_video_to_raw_consumer() {
        let plan = DebugRecordingPlan::from_flags(true, true, true);

        assert!(plan.analysis_csv);
        assert!(plan.raw_video);
        assert!(!plan.raw_csv);
    }

    #[test]
    fn debug_recording_disables_all_sinks_when_master_flag_is_off() {
        let plan = DebugRecordingPlan::from_flags(false, true, true);

        assert!(!plan.analysis_csv);
        assert!(!plan.raw_video);
        assert!(!plan.raw_csv);
    }

    #[test]
    fn debug_recorder_writes_analysis_csv_rows_when_result_is_recorded() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let output_dir = std::env::temp_dir().join(format!("ruler-debug-recorder-{unique}"));
        std::fs::create_dir_all(&output_dir).unwrap();
        let csv_path = output_dir.join("analysis.csv");
        let mut recorder =
            DebugRecorder::start(&output_dir, false, true, 1280, 720, PixelFormat::Rgba).unwrap();
        let result = FrameResult {
            logical_frame: Some(12),
            total_frames_in_cycle: 30,
            raw_pixel_width: Some(42),
            elapsed_frames: 12,
            cost_is_negative: false,
            battle_state: BattleState::OneXRunning,
            timing_debug: None,
        };

        recorder.record_analysis_row(&result, 345);
        drop(recorder);

        let csv = std::fs::read_to_string(&csv_path).unwrap();
        let _ = std::fs::remove_dir_all(&output_dir);

        assert_eq!(csv.lines().count(), 2);
        assert!(csv.contains(",42,12,30,0,12,345,0.400000,1x_running,"));
    }
}
