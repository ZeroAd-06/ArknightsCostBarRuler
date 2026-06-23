//! Frame types shared between the capture pipeline and its consumers.
//!
//! A [`Frame`] is the unit of work that flows through Layer 1. Each frame
//! carries an opaque binary pixel buffer plus enough metadata for downstream
//! analysis (dimensions, format, capture timing).

use std::sync::Arc;

use crate::analysis::scanner::PixelFormat;

/// Monotonic frame identifier assigned by the capture pipeline. The first
/// frame produced after pipeline start has id `0`; subsequent frames increment
/// by one. Frame ids are never reused within a single pipeline run.
pub type FrameId = u64;

/// Wire-format tag for [`PixelFormat`]. Stable across versions so external
/// named-pipe consumers can decode frames without depending on ruler-core.
pub fn pixel_format_tag(format: PixelFormat) -> u32 {
    match format {
        PixelFormat::Rgba => 0,
        PixelFormat::Bgr => 1,
    }
}

/// Inverse of [`pixel_format_tag`]. Returns `None` for unknown tags.
pub fn pixel_format_from_tag(tag: u32) -> Option<PixelFormat> {
    match tag {
        0 => Some(PixelFormat::Rgba),
        1 => Some(PixelFormat::Bgr),
        _ => None,
    }
}

/// Number of bytes per pixel for a given format.
pub fn bytes_per_pixel(format: PixelFormat) -> u32 {
    match format {
        PixelFormat::Rgba => 4,
        PixelFormat::Bgr => 3,
    }
}

/// Total bytes occupied by a frame's pixel buffer.
pub fn frame_byte_size(width: u32, height: u32, format: PixelFormat) -> usize {
    (width as usize) * (height as usize) * (bytes_per_pixel(format) as usize)
}

/// A captured frame ready for analysis or recording.
///
/// The pixel buffer is held behind an [`Arc`] so that multiple consumers can
/// reference the same allocation without copying. When the in-memory store
/// spills a frame to disk, the [`Arc`] is dropped and the buffer is recreated
/// from disk on demand.
#[derive(Clone)]
pub struct Frame {
    pub id: FrameId,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Capture duration in microseconds — how long the backend spent producing
    /// this frame. Forwarded to debug recorders for latency analysis.
    pub capture_duration_us: u64,
    /// Monotonic capture timestamp in nanoseconds since pipeline start. Used
    /// only for diagnostic logging; not wall-clock time.
    pub capture_timestamp_ns: u64,
    pub data: Arc<Vec<u8>>,
}

impl Frame {
    /// Number of bytes in `data`. Equivalent to
    /// `frame_byte_size(self.width, self.height, self.format)`.
    pub fn byte_size(&self) -> usize {
        frame_byte_size(self.width, self.height, self.format)
    }

    /// Approximate heap size of this frame, including the pixel buffer.
    /// Used by the [`FrameStore`](super::store::FrameStore) to decide when to
    /// spill to disk.
    pub fn approx_heap_bytes(&self) -> usize {
        // Arc overhead + Vec header + pixel data; we only count the pixel data
        // precisely because the rest is constant and negligible.
        self.byte_size() + 64
    }
}

/// Metadata for a frame whose pixel buffer has been spilled to disk. The
/// pixel data can be reloaded by reading `spill_path` and reconstructing the
/// `Arc<Vec<u8>>`.
#[derive(Clone, Debug)]
pub struct SpilledFrame {
    pub id: FrameId,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub capture_duration_us: u64,
    pub capture_timestamp_ns: u64,
    pub spill_path: std::path::PathBuf,
}

impl SpilledFrame {
    pub fn byte_size(&self) -> usize {
        frame_byte_size(self.width, self.height, self.format)
    }

    /// Reload the pixel buffer from disk. Returns the recreated [`Frame`].
    pub fn reload(self) -> std::io::Result<Frame> {
        let bytes = std::fs::read(&self.spill_path)?;
        Ok(Frame {
            id: self.id,
            width: self.width,
            height: self.height,
            format: self.format,
            capture_duration_us: self.capture_duration_us,
            capture_timestamp_ns: self.capture_timestamp_ns,
            data: Arc::new(bytes),
        })
    }
}
