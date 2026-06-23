//! Three-layer architecture — Layer 1: capture pipeline.
//!
//! The [`CapturePipeline`] owns the capture backend, runs a dedicated capture
//! thread, and exposes a Windows named pipe endpoint that any number of
//! consumers can connect to. Each consumer has a forward-only cursor and a
//! policy (`InOrder` or `SkipToLatest`) that controls delivery semantics.
//!
//! Frames are held in an in-memory [`FrameStore`] until all consumers have
//! finished processing them. When the in-memory footprint exceeds the
//! configured threshold (default 512 MiB), the oldest still-needed frames are
//! spilled to disk and reloaded on demand.
//!
//! Threading model
//! ---------------
//! - **Capture thread**: captures frames from the backend, assigns monotonic
//!   ids, pushes them to the `FrameStore`, updates the "latest frame id"
//!   atomic, and notifies consumer sender threads via a condvar.
//! - **Accept thread**: accepts new named pipe connections, reads the
//!   subscribe packet, registers the consumer in the registry, and spawns a
//!   sender thread.
//! - **Sender thread** (one per consumer): for `InOrder`, pushes frames in
//!   order and waits for acks; for `SkipToLatest`, waits for `RPUL` from the
//!   client and responds with the latest available frame.
//! - **Janitor thread**: periodically recomputes the minimum acked cursor
//!   across all consumers, releases consumed frames from the `FrameStore`,
//!   and triggers disk spill if over threshold.
//!
//! Public API
//! ----------
//! - [`CapturePipeline::start`] — start the pipeline.
//! - [`CapturePipeline::connect_consumer`] — connect an in-process consumer.
//! - [`CapturePipeline::pipe_name`] — the named pipe path (for external
//!   consumers to connect to).
//! - [`CapturePipeline::shutdown`] — stop the pipeline.

pub mod cursor;
pub mod frame;
#[cfg(windows)]
pub mod pipe;
pub mod store;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::capture::{create_backend, CaptureBackend, CaptureConfig, CapturedFrame, WindowInfo};
use crate::pipeline::cursor::{ConsumerPolicy, Cursor, CursorRegistry};
use crate::pipeline::frame::{Frame, FrameId};
use crate::pipeline::store::{FrameStore, SpillConfig, StoreError};

#[cfg(windows)]
pub use crate::pipeline::pipe::{
    ConsumerPipe, PipeError, PipeHandle, PipeServer, protocol,
};

/// Errors that can occur while starting or running the pipeline.
#[derive(Debug)]
pub enum PipelineError {
    Backend(String),
    Store(StoreError),
    #[cfg(windows)]
    Pipe(PipeError),
    Io(std::io::Error),
    /// Pipeline is shutting down or already shut down.
    Shutdown,
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineError::Backend(msg) => write!(f, "capture backend error: {msg}"),
            PipelineError::Store(err) => write!(f, "frame store error: {err}"),
            #[cfg(windows)]
            PipelineError::Pipe(err) => write!(f, "pipe error: {err}"),
            PipelineError::Io(err) => write!(f, "I/O error: {err}"),
            PipelineError::Shutdown => write!(f, "pipeline shutting down"),
        }
    }
}

impl std::error::Error for PipelineError {}

impl From<StoreError> for PipelineError {
    fn from(value: StoreError) -> Self {
        PipelineError::Store(value)
    }
}

impl From<std::io::Error> for PipelineError {
    fn from(value: std::io::Error) -> Self {
        PipelineError::Io(value)
    }
}

#[cfg(windows)]
impl From<PipeError> for PipelineError {
    fn from(value: PipeError) -> Self {
        PipelineError::Pipe(value)
    }
}

/// Configuration for starting a [`CapturePipeline`].
#[derive(Clone, Debug)]
pub struct PipelineConfig {
    pub capture: CaptureConfig,
    /// Directory to write spilled frame files into. Created on first spill.
    pub spill_dir: PathBuf,
    /// Maximum total in-memory bytes before spilling begins. Defaults to
    /// 512 MiB.
    pub max_in_memory_bytes: usize,
    /// Unique session identifier used to derive the named pipe name. Should
    /// be unique per running process to avoid collisions.
    pub session_id: String,
}

impl PipelineConfig {
    pub fn new(capture: CaptureConfig, spill_dir: PathBuf, session_id: String) -> Self {
        Self {
            capture,
            spill_dir,
            max_in_memory_bytes: SpillConfig::DEFAULT_MAX_BYTES,
            session_id,
        }
    }
}

/// Information returned by [`CapturePipeline::start`] — the dimensions of the
/// capture target and the pipe name for external consumers.
#[derive(Clone, Debug)]
pub struct PipelineInfo {
    pub width: u32,
    pub height: u32,
    pub window_info: Option<WindowInfo>,
    pub pipe_name: String,
}

/// Internal per-consumer state shared between the accept thread, sender
/// thread, and janitor.
#[allow(dead_code)]
struct ConsumerEntry {
    cursor: Arc<Cursor>,
    /// Latest frame id that the capture thread has produced. The sender
    /// thread reads this to decide whether to send a new frame.
    latest_seen: AtomicU64,
}

struct PipelineInner {
    store: FrameStore,
    /// Monotonically increasing frame id counter. The next captured frame
    /// gets this id.
    next_frame_id: AtomicU64,
    /// Highest frame id produced so far. Read by SkipToLatest senders.
    latest_frame_id: AtomicU64,
    /// Per-consumer state, keyed by an arbitrary consumer id (assigned at
    /// registration time).
    consumers: Mutex<HashMap<u64, Arc<ConsumerEntry>>>,
    /// Condvar signalled whenever a new frame is pushed. Sender threads wait
    /// on this.
    frame_available: Condvar,
    frame_available_lock: Mutex<()>,
    /// Condvar signalled whenever a consumer's cursor advances. The janitor
    /// waits on this (with timeout) to know when to attempt release.
    cursor_advanced: Condvar,
    cursor_advanced_lock: Mutex<()>,
    /// Aggregate cursor registry, refreshed by the janitor. Reserved for
    /// future observability endpoints.
    #[allow(dead_code)]
    cursor_registry: CursorRegistry,
    shutdown: AtomicBool,
    #[cfg(windows)]
    pipe_server: Option<PipeServer>,
}

impl PipelineInner {
    fn notify_frame_available(&self) {
        let _lock = self.frame_available_lock.lock().expect("frame_available lock poisoned");
        self.frame_available.notify_all();
    }

    fn wait_for_new_frame(&self, after_id: FrameId) {
        // Spin until either a new frame arrives or shutdown.
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                return;
            }
            if self.latest_frame_id.load(Ordering::Acquire) > after_id {
                return;
            }
            let lock = self
                .frame_available_lock
                .lock()
                .expect("frame_available lock poisoned");
            if self.latest_frame_id.load(Ordering::Acquire) > after_id {
                return;
            }
            let _ = self
                .frame_available
                .wait_timeout(lock, Duration::from_millis(50))
                .expect("frame_available wait poisoned");
        }
    }

    fn notify_cursor_advanced(&self) {
        let _lock = self.cursor_advanced_lock.lock().expect("cursor_advanced lock poisoned");
        self.cursor_advanced.notify_all();
    }
}

/// The Layer 1 capture pipeline. Owns the capture backend and the named pipe
/// server, and runs the capture/accept/janitor threads.
pub struct CapturePipeline {
    inner: Arc<PipelineInner>,
    info: PipelineInfo,
    capture_thread: Option<JoinHandle<()>>,
    accept_thread: Option<JoinHandle<()>>,
    janitor_thread: Option<JoinHandle<()>>,
}

impl CapturePipeline {
    /// Start the pipeline. Connects to the capture backend, creates the
    /// frame store and named pipe server, and spawns the capture/accept/
    /// janitor threads.
    pub fn start(config: PipelineConfig) -> Result<(Self, PipelineInfo), PipelineError> {
        // Connect backend.
        let mut backend = create_backend(config.capture.clone())
            .map_err(PipelineError::Backend)?;
        backend.connect().map_err(PipelineError::Backend)?;
        let dims = backend.dimensions();
        let window_info = backend.window_info();
        log::info!(
            "capture pipeline connected: dimensions={}x{}, window_info={:?}",
            dims.0,
            dims.1,
            window_info
        );

        // Create frame store.
        let spill_config = SpillConfig {
            spill_dir: config.spill_dir.clone(),
            max_in_memory_bytes: config.max_in_memory_bytes,
        };
        let store = FrameStore::new(spill_config)?;

        // Create named pipe server (Windows only).
        #[cfg(windows)]
        let pipe_server = PipeServer::new(&config.session_id)?;
        #[cfg(windows)]
        let pipe_name = pipe_server.pipe_name().to_string();
        #[cfg(not(windows))]
        let pipe_name = String::new();

        let inner = Arc::new(PipelineInner {
            store,
            next_frame_id: AtomicU64::new(0),
            latest_frame_id: AtomicU64::new(0),
            consumers: Mutex::new(HashMap::new()),
            frame_available: Condvar::new(),
            frame_available_lock: Mutex::new(()),
            cursor_advanced: Condvar::new(),
            cursor_advanced_lock: Mutex::new(()),
            cursor_registry: CursorRegistry::new(),
            shutdown: AtomicBool::new(false),
            #[cfg(windows)]
            pipe_server: Some(pipe_server),
        });

        let info = PipelineInfo {
            width: dims.0,
            height: dims.1,
            window_info,
            pipe_name: pipe_name.clone(),
        };

        let pipeline = CapturePipeline {
            inner: Arc::clone(&inner),
            info: info.clone(),
            capture_thread: None,
            accept_thread: None,
            janitor_thread: None,
        };

        // Spawn capture thread.
        let capture_inner = Arc::clone(&inner);
        let capture_handle = thread::Builder::new()
            .name("ruler-pipeline-capture".to_string())
            .spawn(move || {
                run_capture_loop(capture_inner, backend);
            })
            .map_err(|e| PipelineError::Io(io_err_from("capture thread", e)))?;

        // Spawn accept thread (Windows only).
        #[cfg(windows)]
        let accept_handle = {
            let accept_inner = Arc::clone(&inner);
            Some(
                thread::Builder::new()
                    .name("ruler-pipeline-accept".to_string())
                    .spawn(move || {
                        run_accept_loop(accept_inner);
                    })
                    .map_err(|e| PipelineError::Io(io_err_from("accept thread", e)))?,
            )
        };
        #[cfg(not(windows))]
        let accept_handle: Option<JoinHandle<()>> = None;

        // Spawn janitor thread.
        let janitor_inner = Arc::clone(&inner);
        let janitor_handle = thread::Builder::new()
            .name("ruler-pipeline-janitor".to_string())
            .spawn(move || {
                run_janitor_loop(janitor_inner);
            })
            .map_err(|e| PipelineError::Io(io_err_from("janitor thread", e)))?;

        let mut pipeline = pipeline;
        pipeline.capture_thread = Some(capture_handle);
        pipeline.accept_thread = accept_handle;
        pipeline.janitor_thread = Some(janitor_handle);

        Ok((pipeline, info))
    }

    pub fn info(&self) -> &PipelineInfo {
        &self.info
    }

    pub fn pipe_name(&self) -> &str {
        &self.info.pipe_name
    }

    /// The id of the most recently produced frame. Useful for connecting an
    /// `InOrder` consumer that should start from "now" rather than from the
    /// beginning of the (possibly long-since-released) stream.
    pub fn latest_frame_id(&self) -> FrameId {
        self.inner.latest_frame_id.load(Ordering::Acquire)
    }

    /// Connect an in-process consumer with the given policy.
    ///
    /// For `SkipToLatest` consumers, `start_frame_id` is typically 0 (start
    /// from the next produced frame). For `InOrder` consumers, `start_frame_id`
    /// is the first frame id the consumer wants to receive.
    ///
    /// Only available on Windows — the named pipe transport is Windows-only.
    #[cfg(windows)]
    pub fn connect_consumer(
        &self,
        policy: ConsumerPolicy,
        start_frame_id: FrameId,
    ) -> Result<ConsumerPipe, PipelineError> {
        ConsumerPipe::connect(&self.info.pipe_name, policy, start_frame_id)
            .map_err(PipelineError::Io)
    }

    /// Shut down the pipeline. Signals all threads to stop and joins them.
    pub fn shutdown(&mut self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.inner.notify_frame_available();
        self.inner.notify_cursor_advanced();
        #[cfg(windows)]
        if let Some(server) = &self.inner.pipe_server {
            server.shutdown();
        }

        if let Some(handle) = self.capture_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.janitor_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for CapturePipeline {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn io_err_from(ctx: &str, e: std::io::Error) -> std::io::Error {
    std::io::Error::other(format!("failed to spawn {ctx}: {e}"))
}

/// Capture loop: continuously captures frames from the backend and pushes
/// them to the frame store.
fn run_capture_loop(inner: Arc<PipelineInner>, mut backend: Box<dyn CaptureBackend>) {
    let pipeline_start = Instant::now();
    while !inner.shutdown.load(Ordering::Relaxed) {
        let capture_start = Instant::now();
        let captured: CapturedFrame = match backend.capture_frame() {
            Ok(frame) => frame,
            Err(error) => {
                if inner.shutdown.load(Ordering::Relaxed) {
                    break;
                }
                log::error!("capture pipeline: backend error: {error}");
                thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        let capture_dur_us = capture_start.elapsed().as_micros() as u64;
        let timestamp_ns = pipeline_start.elapsed().as_nanos() as u64;

        let frame_id = inner.next_frame_id.fetch_add(1, Ordering::SeqCst);
        let frame = Frame {
            id: frame_id,
            width: captured.width,
            height: captured.height,
            format: captured.format,
            capture_duration_us: capture_dur_us,
            capture_timestamp_ns: timestamp_ns,
            data: std::sync::Arc::new(captured.data),
        };

        inner.store.push(frame);
        inner
            .latest_frame_id
            .store(frame_id, Ordering::Release);
        inner.notify_frame_available();
        inner.notify_cursor_advanced(); // a new frame may allow release of older ones... no, release happens when cursors advance
    }
    log::info!("capture pipeline: capture loop exiting");
    backend.disconnect();
}

/// Accept loop (Windows only): accepts new named pipe connections and
/// spawns a sender thread per consumer.
#[cfg(windows)]
fn run_accept_loop(inner: Arc<PipelineInner>) {
    let server = match &inner.pipe_server {
        Some(s) => s,
        None => return,
    };
    let mut next_consumer_id: u64 = 0;
    while !inner.shutdown.load(Ordering::Relaxed) {
        let (pipe, policy, start_frame_id) = match server.accept() {
            Ok(triple) => triple,
            Err(PipeError::Shutdown) => break,
            Err(error) => {
                log::error!("capture pipeline: accept error: {error}");
                thread::sleep(Duration::from_millis(50));
                continue;
            }
        };

        let consumer_id = next_consumer_id;
        next_consumer_id += 1;
        let cursor = Arc::new(Cursor::new(policy, start_frame_id));
        let entry = Arc::new(ConsumerEntry {
            cursor: Arc::clone(&cursor),
            latest_seen: AtomicU64::new(start_frame_id),
        });

        inner
            .consumers
            .lock()
            .expect("consumers poisoned")
            .insert(consumer_id, Arc::clone(&entry));

        let sender_inner = Arc::clone(&inner);
        let sender_cursor = Arc::clone(&cursor);
        let cleanup_inner = Arc::clone(&inner);
        let _ = thread::Builder::new()
            .name(format!("ruler-pipeline-sender-{consumer_id}"))
            .spawn(move || {
                run_sender_loop(sender_inner, pipe, policy, sender_cursor);
                // Remove ourselves from the registry on exit.
                cleanup_inner
                    .consumers
                    .lock()
                    .expect("consumers poisoned")
                    .remove(&consumer_id);
                cleanup_inner.notify_cursor_advanced();
            });
    }
    log::info!("capture pipeline: accept loop exiting");
}

/// Sender loop (Windows only): per-consumer thread that delivers frames over
/// the named pipe.
#[cfg(windows)]
fn run_sender_loop(
    inner: Arc<PipelineInner>,
    mut pipe: PipeHandle,
    policy: ConsumerPolicy,
    cursor: Arc<Cursor>,
) {
    match policy {
        ConsumerPolicy::InOrder => run_in_order_sender(&inner, &mut pipe, &cursor),
        ConsumerPolicy::SkipToLatest => run_skip_to_latest_sender(&inner, &mut pipe, &cursor),
    }
}

#[cfg(windows)]
fn run_in_order_sender(
    inner: &PipelineInner,
    pipe: &mut PipeHandle,
    cursor: &Cursor,
) {
    while !inner.shutdown.load(Ordering::Relaxed) {
        let next_id = cursor.next_to_deliver();
        // Wait until a frame with id >= next_id exists.
        inner.wait_for_new_frame(next_id.saturating_sub(1));
        if inner.shutdown.load(Ordering::Relaxed) {
            break;
        }
        let latest = inner.latest_frame_id.load(Ordering::Acquire);
        if next_id > latest {
            continue;
        }

        let frame = match inner.store.get(next_id) {
            Ok(frame) => frame,
            Err(StoreError::NotFound(_)) => {
                // `next_id` was released before we could deliver it: the
                // consumer started behind the store's retention window (e.g. a
                // calibration consumer joining a long-running pipeline), or
                // every consumer acked past it during a release cycle. Frames
                // form a contiguous range, so a missing `next_id` is older than
                // the oldest retained frame — skip forward to that frame rather
                // than tearing down the connection with an error.
                if let Some(oldest) = inner.store.oldest_id() {
                    log::debug!(
                        "capture pipeline: in-order frame {next_id} already released; \
                         skipping forward to {oldest}"
                    );
                    cursor.record_ack(oldest.saturating_sub(1));
                    cursor.mark_delivered(oldest);
                }
                continue;
            }
            Err(err) => {
                log::error!("capture pipeline: frame {next_id} unavailable: {err}");
                let _ = pipe::write_error(pipe, &format!("frame {next_id} unavailable"));
                break;
            }
        };

        if let Err(err) = pipe::write_frame(pipe, &frame) {
            log::debug!("capture pipeline: in-order sender write failed: {err}");
            break;
        }

        // Wait for ack.
        match pipe::read_ack(pipe) {
            Ok(acked_id) => {
                cursor.mark_delivered(acked_id + 1);
                cursor.record_ack(acked_id);
                inner.notify_cursor_advanced();
            }
            Err(err) => {
                log::debug!("capture pipeline: in-order sender ack failed: {err}");
                break;
            }
        }
    }
    log::debug!("capture pipeline: in-order sender exiting");
}

#[cfg(windows)]
fn run_skip_to_latest_sender(
    inner: &PipelineInner,
    pipe: &mut PipeHandle,
    cursor: &Cursor,
) {
    while !inner.shutdown.load(Ordering::Relaxed) {
        // Wait for the client to ask for a frame.
        match pipe::read_pull(pipe) {
            Ok(()) => {}
            Err(pipe::PipeError::Shutdown) | Err(pipe::PipeError::Io(_)) => break,
            Err(err) => {
                log::debug!("capture pipeline: skip-to-latest sender read_pull failed: {err}");
                break;
            }
        }

        let last_delivered = cursor.next_to_deliver().saturating_sub(1);
        // Wait until a frame newer than last_delivered exists.
        inner.wait_for_new_frame(last_delivered);
        if inner.shutdown.load(Ordering::Relaxed) {
            let _ = pipe::write_no_frame(pipe);
            break;
        }

        let latest = inner.latest_frame_id.load(Ordering::Acquire);
        if latest <= last_delivered {
            // Spurious wakeup; loop and wait for next RPUL.
            continue;
        }

        let frame = match inner.store.get(latest) {
            Ok(frame) => frame,
            Err(err) => {
                log::error!("capture pipeline: latest frame {latest} unavailable: {err}");
                let _ = pipe::write_error(pipe, &format!("frame {latest} unavailable"));
                break;
            }
        };

        if let Err(err) = pipe::write_frame(pipe, &frame) {
            log::debug!("capture pipeline: skip-to-latest sender write failed: {err}");
            break;
        }

        // SkipToLatest auto-advances both delivered and acked to the latest.
        cursor.mark_delivered(latest + 1);
        cursor.record_ack(latest);
        inner.notify_cursor_advanced();
    }
    log::debug!("capture pipeline: skip-to-latest sender exiting");
}

/// Janitor loop: periodically releases consumed frames and triggers disk
/// spill if over the memory threshold.
fn run_janitor_loop(inner: Arc<PipelineInner>) {
    while !inner.shutdown.load(Ordering::Relaxed) {
        // Recompute min acked across all consumers.
        let acked_ids: Vec<FrameId> = inner
            .consumers
            .lock()
            .expect("consumers poisoned")
            .values()
            .map(|entry| entry.cursor.acked())
            .collect();

        if let Some(min_acked) = acked_ids.iter().copied().min() {
            inner.store.release_before(min_acked);
        } else {
            // No consumers — release everything older than the latest.
            let latest = inner.latest_frame_id.load(Ordering::Acquire);
            if latest > 0 {
                inner.store.release_before(latest - 1);
            }
        }

        // Spill if over threshold.
        match inner.store.maybe_spill() {
            Ok(count) if count > 0 => {
                log::debug!("capture pipeline: spilled {count} frames to disk");
            }
            Ok(_) => {}
            Err(err) => {
                log::warn!("capture pipeline: spill error: {err}");
            }
        }

        // Wait for the next cursor-advance notification or a timeout.
        let lock = inner
            .cursor_advanced_lock
            .lock()
            .expect("cursor_advanced lock poisoned");
        let _ = inner
            .cursor_advanced
            .wait_timeout(lock, Duration::from_millis(500))
            .expect("cursor_advanced wait poisoned");
    }
    log::info!("capture pipeline: janitor loop exiting");
}
