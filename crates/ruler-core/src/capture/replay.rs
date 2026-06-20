use std::{
    io::Read,
    process::{Child, Command, Stdio},
    time::Instant,
};

use crate::analysis::scanner::PixelFormat;
use crate::capture::{CaptureBackend, CapturedFrame};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn bytes_per_pixel(fmt: PixelFormat) -> u32 {
    match fmt {
        PixelFormat::Rgba => 4,
        PixelFormat::Bgr => 3,
    }
}

/// Flip top-down rows to bottom-up (the scanner expects bottom-up).
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
// ReplayCaptureBackend
// ---------------------------------------------------------------------------

/// Virtual capture backend that replays frames from a pre-recorded video file.
///
/// On `connect()` it spawns `ffmpeg` to decode the recorded video file to raw video
/// frames via pipe.  Each `capture_frame()` call:
///
/// 1. Computes the expected frame index: `elapsed_seconds × target_fps`
/// 2. Advances the decode pipe to that frame (discarding intermediate frames)
/// 3. Reads one frame, flips it bottom-up (for the scanner), and returns it
///
/// This simulates real-time capture at the configured `target_fps`.
pub struct ReplayCaptureBackend {
    hevc_path: String,
    ffmpeg: Option<Child>,
    stdout: Option<std::process::ChildStdout>,
    width: u32,
    height: u32,
    bpp: u32,
    pix_fmt: PixelFormat,
    target_fps: f64,
    start_time: Instant,
    /// Number of frames consumed from the pipe so far.
    frames_read: u64,
    /// Reusable single-frame buffer.
    frame_buf: Vec<u8>,
    frame_size: usize,
}

impl ReplayCaptureBackend {
    /// Create a new replay backend.
    ///
    /// * `hevc_path` – path to the recorded video file to replay
    /// * `target_fps` – playback frame rate (e.g. `60.0`)
    /// * `width`, `height` – video dimensions (obtain e.g. via `ffprobe`)
    /// * `pix_fmt` – pixel format to decode to (default: `Rgba`)
    #[must_use]
    pub fn new(
        hevc_path: &str,
        target_fps: f64,
        width: u32,
        height: u32,
        pix_fmt: PixelFormat,
    ) -> Self {
        let bpp = bytes_per_pixel(pix_fmt);
        let frame_size = (width * height * bpp) as usize;
        Self {
            hevc_path: hevc_path.to_string(),
            ffmpeg: None,
            stdout: None,
            width,
            height,
            bpp,
            pix_fmt,
            target_fps,
            start_time: Instant::now(),
            frames_read: 0,
            frame_buf: vec![0u8; frame_size],
            frame_size,
        }
    }

    /// Read exactly one frame from the pipe into `self.frame_buf`.
    /// Returns `true` on success, `false` on EOF or error.
    fn read_into_buf(&mut self) -> bool {
        match self.stdout.as_mut() {
            Some(stdout) => stdout.read_exact(&mut self.frame_buf).is_ok(),
            None => false,
        }
    }

    /// Skip (read and discard) `n` frames from the pipe.
    fn skip_frames(&mut self, n: u64) {
        let stdout = match self.stdout.as_mut() {
            Some(s) => s,
            None => return,
        };
        let mut discard = vec![0u8; self.frame_size.min(65536)];
        for _ in 0..n {
            let mut rem = self.frame_size;
            while rem > 0 {
                let to_read = rem.min(discard.len());
                match stdout.read(&mut discard[..to_read]) {
                    Ok(0) => return, // EOF
                    Ok(got) => rem -= got,
                    Err(_) => return, // I/O error
                }
            }
        }
    }
}

impl CaptureBackend for ReplayCaptureBackend {
    fn connect(&mut self) -> Result<(), String> {
        log::info!(
            "replay connect: path='{}', target_fps={}",
            self.hevc_path,
            self.target_fps
        );
        let pix_fmt_str = match self.pix_fmt {
            PixelFormat::Rgba => "rgba",
            PixelFormat::Bgr => "bgr24",
        };

        let mut child = Command::new("ffmpeg")
            .args([
                "-i",
                &self.hevc_path,
                "-fps_mode",
                "passthrough",
                "-f",
                "rawvideo",
                "-pixel_format",
                pix_fmt_str,
                "-video_size",
                &format!("{}x{}", self.width, self.height),
                "-an",
                "-sn",
                "-dn",
                "pipe:0",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn ffmpeg decoder: {e}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "ffmpeg stdout not available".to_string())?;

        self.ffmpeg = Some(child);
        self.stdout = Some(stdout);
        self.start_time = Instant::now();
        self.frames_read = 0;

        Ok(())
    }

    fn capture_frame(&mut self) -> Result<CapturedFrame, String> {
        // Which frame does the clock say we should be at?
        let elapsed = self.start_time.elapsed();
        let expected = (elapsed.as_secs_f64() * self.target_fps) as u64;

        // Catch up: discard intermediate frames
        if expected > self.frames_read {
            self.skip_frames(expected - self.frames_read);
            self.frames_read = expected;
        }

        // Read the frame at the expected position
        if !self.read_into_buf() {
            return Err("replay: end of recorded video file reached".to_string());
        }
        self.frames_read += 1;

        // Flip top-down → bottom-up for the scanner
        flip_rows(&mut self.frame_buf, self.width, self.height, self.bpp);

        Ok(CapturedFrame {
            data: self.frame_buf.clone(),
            width: self.width,
            height: self.height,
            format: self.pix_fmt,
        })
    }

    fn disconnect(&mut self) {
        log::info!("replay disconnect: path='{}'", self.hevc_path);
        self.stdout.take();
        if let Some(mut child) = self.ffmpeg.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}
