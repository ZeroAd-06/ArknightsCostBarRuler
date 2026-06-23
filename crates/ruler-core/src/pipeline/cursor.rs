//! Consumer policy and cursor tracking.
//!
//! Every consumer registered with the capture pipeline has an associated
//! [`ConsumerPolicy`] that controls how the pipeline delivers frames to it,
//! and a [`Cursor`] that records the consumer's position in the frame stream.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use super::frame::FrameId;

/// Per-consumer delivery policy.
///
/// `InOrder` consumers receive every frame in capture order, with backpressure
/// — the pipeline will not skip frames even if the consumer falls behind.
/// `SkipToLatest` consumers only ever receive the most recent frame at the
/// moment they ask for the next one; intermediate frames are dropped on the
/// floor. This is appropriate for real-time consumers like the analysis layer
/// that care about latency, not completeness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsumerPolicy {
    InOrder,
    SkipToLatest,
}

impl ConsumerPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            ConsumerPolicy::InOrder => "in-order",
            ConsumerPolicy::SkipToLatest => "skip-to-latest",
        }
    }
}

/// Tracks the position of a single consumer in the frame stream.
///
/// Two positions are tracked:
/// - `delivered`: the id of the most recent frame that has been *sent* to this
///   consumer's pipe. The pipeline will not re-send any frame with id <= this.
/// - `acked`: the id of the most recent frame the consumer has *finished*
///   processing and signalled back via [`ConsumerHandle::ack`](super::ConsumerHandle::ack).
///   The [`FrameStore`](super::store::FrameStore) may release any frame with
///   id <= `min(all consumers' acked)`.
///
/// For `SkipToLatest` consumers, `delivered` is updated atomically whenever a
/// new frame is produced (without waiting for the consumer to ask), and `acked`
/// is updated to `delivered` immediately — the consumer has no way to signal
/// partial processing because it only ever sees the latest frame.
///
/// For `InOrder` consumers, `delivered` advances one frame at a time as the
/// pipeline writes to the pipe, and `acked` advances when the consumer sends
/// an ack packet.
pub struct Cursor {
    policy: ConsumerPolicy,
    /// Next frame id to deliver to this consumer. Equivalent to
    /// `delivered + 1` in steady state.
    next_to_deliver: AtomicU64,
    /// Highest frame id the consumer has acknowledged. The FrameStore may
    /// release frames with id <= this value (subject to other consumers).
    acked: Mutex<FrameId>,
}

impl Cursor {
    pub fn new(policy: ConsumerPolicy, start_frame_id: FrameId) -> Self {
        Self {
            policy,
            next_to_deliver: AtomicU64::new(start_frame_id),
            acked: Mutex::new(start_frame_id.saturating_sub(1)),
        }
    }

    pub fn policy(&self) -> ConsumerPolicy {
        self.policy
    }

    /// The id of the next frame this consumer should receive.
    pub fn next_to_deliver(&self) -> FrameId {
        self.next_to_deliver.load(Ordering::Acquire)
    }

    /// Advance `next_to_deliver` to the given frame id, used when a frame has
    /// been written to the consumer's pipe. The new value must be >= the
    /// current value.
    pub fn mark_delivered(&self, frame_id: FrameId) {
        self.next_to_deliver.store(frame_id, Ordering::Release);
    }

    /// For `SkipToLatest` consumers: jump `next_to_deliver` straight to the
    /// newest frame id, discarding any intermediate frames. Returns the id
    /// that should actually be sent (the latest), or `None` if the cursor is
    /// already at or past `latest`.
    pub fn skip_to_latest(&self, latest: FrameId) -> Option<FrameId> {
        debug_assert_eq!(
            self.policy,
            ConsumerPolicy::SkipToLatest,
            "skip_to_latest called on InOrder consumer"
        );
        let current = self.next_to_deliver.load(Ordering::Acquire);
        if current > latest {
            return None;
        }
        // Even if another thread races us and also sets the cursor, the latest
        // frame is idempotent — both threads will compute the same value.
        self.next_to_deliver.store(latest + 1, Ordering::Release);
        // For SkipToLatest, acked advances immediately to the delivered frame
        // because the consumer cannot signal partial processing.
        let mut acked = self.acked.lock().expect("cursor acked poisoned");
        if latest > *acked {
            *acked = latest;
        }
        Some(latest)
    }

    /// Record an ack from an InOrder consumer. The frame id must be >= the
    /// current acked value.
    pub fn record_ack(&self, frame_id: FrameId) {
        let mut acked = self.acked.lock().expect("cursor acked poisoned");
        if frame_id > *acked {
            *acked = frame_id;
        }
    }

    /// The highest frame id this consumer has finished processing. Frames
    /// with id <= this value are safe to release (subject to other consumers).
    pub fn acked(&self) -> FrameId {
        *self.acked.lock().expect("cursor acked poisoned")
    }
}

/// Aggregate cursor state across all consumers, used by the FrameStore to
/// decide which frames can be released.
#[derive(Default)]
pub struct CursorRegistry {
    cursors: Mutex<Vec<FrameId>>,
}

impl CursorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// (Re)populate the registry with the given list of acked positions.
    /// Called by the pipeline on every release-decision tick.
    pub fn refresh(&self, acked_ids: Vec<FrameId>) {
        *self.cursors.lock().expect("cursor registry poisoned") = acked_ids;
    }

    /// The minimum acked frame id across all registered consumers. Frames
    /// with id <= this value may be released. Returns `None` if there are no
    /// consumers (in which case frames can be released immediately).
    pub fn min_acked(&self) -> Option<FrameId> {
        self.cursors
            .lock()
            .expect("cursor registry poisoned")
            .iter()
            .copied()
            .min()
    }
}
