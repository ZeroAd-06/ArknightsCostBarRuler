//! Debug recorder — background capture of video + analysis data for debugging.
//!
//! Controlled exclusively via config.json (no UI). When `debug_recording_enabled`
//! is true, the worker thread writes session-local debug artifacts: raw capture
//! video through a Layer 1 consumer and analysis rows from the Layer 2 analyzer.
//!
//! Both outputs are written to `{session_dir}/capture.mkv` and
//! `{session_dir}/analysis.csv` respectively.
//!
//! In the three-layer architecture, [`DebugRecorderConsumer`] wraps a pipeline
//! `ConsumerPipe` (InOrder policy) and records raw frames only. Analysis CSV
//! rows are written by the analyzer path, which uses SkipToLatest and may drop
//! intermediate frames.

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
              advanced_frames,accumulator_fp,frames_until_next_cost,\
              match_error_px,boundary_corrected\n",
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
        timing: Option<ruler_core::TimingDebug>,
    ) -> std::io::Result<()> {
        let ts_us = self.start.elapsed().as_micros();
        let phase = match (logical_frame, total_frames_in_cycle) {
            (Some(lf), tfc) if tfc > 0 => Some(lf as f64 / tfc as f64),
            _ => None,
        };
        let timing_cols = match timing {
            Some(t) => format!(
                "{},{},{},{},{}",
                t.advanced_frames,
                t.accumulator_fp,
                t.frames_until_next_cost,
                t.match_error_px,
                if t.boundary_corrected { 1 } else { 0 },
            ),
            None => ",,,,".to_string(),
        };

        writeln!(
            self.inner,
            "{},{},{},{},{},{},{},{},{},{},{}",
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
            timing_cols,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DebugRecordingPlan {
    pub raw_video: bool,
    pub analysis_csv: bool,
}

impl DebugRecordingPlan {
    pub fn from_config(enabled: bool, record_video: bool, record_csv: bool) -> Self {
        Self {
            raw_video: enabled && record_video,
            analysis_csv: enabled && record_csv,
        }
    }

    pub fn any(self) -> bool {
        self.raw_video || self.analysis_csv
    }
}

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
            if let Err(e) = csv.write_row(
                result.raw_pixel_width,
                result.logical_frame,
                result.total_frames_in_cycle,
                result.cost_is_negative,
                result.elapsed_frames,
                capture_dur_us,
                result.battle_state,
                result.timing_debug,
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

use ruler_core::pipeline::ConsumerPipe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Configuration for spawning a [`DebugRecorderConsumer`].
pub struct DebugRecorderConfig {
    pub output_dir: PathBuf,
    pub record_video: bool,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

/// A Layer 1 consumer that records every captured frame to video. Runs on its
/// own thread; shutting down the pipeline or dropping this struct stops the
/// thread.
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
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use ruler_core::{BattleState, TimingDebug};

    use super::*;

    fn temp_debug_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ruler-debug-recorder-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp debug dir");
        dir
    }

    #[test]
    fn debug_recording_plan_splits_csv_from_raw_video() {
        assert_eq!(
            DebugRecordingPlan::from_config(false, true, true),
            DebugRecordingPlan {
                raw_video: false,
                analysis_csv: false,
            }
        );
        assert_eq!(
            DebugRecordingPlan::from_config(true, true, false),
            DebugRecordingPlan {
                raw_video: true,
                analysis_csv: false,
            }
        );
        assert_eq!(
            DebugRecordingPlan::from_config(true, false, true),
            DebugRecordingPlan {
                raw_video: false,
                analysis_csv: true,
            }
        );
        assert_eq!(
            DebugRecordingPlan::from_config(true, true, true),
            DebugRecordingPlan {
                raw_video: true,
                analysis_csv: true,
            }
        );
    }

    #[test]
    fn debug_recorder_writes_analysis_rows() {
        let dir = temp_debug_dir("analysis-rows");
        let mut recorder =
            DebugRecorder::start(&dir, false, true, 1, 1, PixelFormat::Rgba).unwrap();
        let result = FrameResult {
            logical_frame: Some(3),
            total_frames_in_cycle: 30,
            raw_pixel_width: Some(12),
            elapsed_frames: 123,
            cost_is_negative: false,
            battle_state: BattleState::OneXRunning,
            timing_debug: Some(TimingDebug {
                required_fp: 0,
                speed_fp: 0,
                accumulator_fp: 100,
                advanced_frames: 2,
                frames_since_cycle_start: 0,
                frames_until_next_cost: 3,
                match_error_px: 4,
                boundary_corrected: true,
            }),
        };

        recorder.record_analysis_row(&result, 456);
        drop(recorder);

        let csv = fs::read_to_string(dir.join("analysis.csv")).unwrap();
        let lines: Vec<_> = csv.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("frame_index,timestamp_ms,raw_pixel_width"));
        assert!(lines[1].ends_with(",12,3,30,0,123,456,0.100000,1x_running,2,100,3,4,1"));

        fs::remove_dir_all(dir).ok();
    }
}
