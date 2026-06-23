//! Frame store with in-memory ring buffer and disk spill.
//!
//! The [`FrameStore`] is the central buffer that holds captured frames until
//! all consumers have finished processing them. When the in-memory footprint
//! exceeds the configured byte threshold, the oldest still-needed frames are
//! spilled to disk and reloaded on demand.
//!
//! Concurrency model
//! -----------------
//! The store is internally synchronized via a single `Mutex`. Contention is
//! low because the only writers are the capture thread (one `push` per frame)
//! and the per-consumer sender threads (one `reload_spilled` per slow
//! consumer). All other operations are reads under the lock.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use super::frame::{Frame, FrameId, SpilledFrame};

/// Configuration for the frame store's disk-spill behaviour.
#[derive(Clone, Debug)]
pub struct SpillConfig {
    /// Directory to write spilled frame files into. Created on first spill.
    pub spill_dir: PathBuf,
    /// Maximum total in-memory bytes before spilling begins. Defaults to
    /// 512 MiB if the caller does not override.
    pub max_in_memory_bytes: usize,
}

impl SpillConfig {
    pub const DEFAULT_MAX_BYTES: usize = 512 * 1024 * 1024;

    pub fn new(spill_dir: PathBuf) -> Self {
        Self {
            spill_dir,
            max_in_memory_bytes: Self::DEFAULT_MAX_BYTES,
        }
    }
}

/// Internal record for a frame currently held by the store. A frame is in
/// exactly one of three states: in-memory, spilled-to-disk, or being reloaded.
#[derive(Clone)]
enum FrameSlot {
    /// Frame is fully in memory and ready to hand out.
    InMemory(Frame),
    /// Frame's pixel buffer has been written to disk; metadata is preserved
    /// so the buffer can be reloaded if a consumer asks for it.
    Spilled(SpilledFrame),
}

impl FrameSlot {
    #[allow(dead_code)]
    fn id(&self) -> FrameId {
        match self {
            FrameSlot::InMemory(f) => f.id,
            FrameSlot::Spilled(s) => s.id,
        }
    }

    #[allow(dead_code)]
    fn approx_heap_bytes(&self) -> usize {
        match self {
            FrameSlot::InMemory(f) => f.approx_heap_bytes(),
            FrameSlot::Spilled(_) => 64, // just the path + metadata
        }
    }
}

pub struct FrameStoreInner {
    slots: BTreeMap<FrameId, FrameSlot>,
    total_in_memory_bytes: usize,
    spill_config: SpillConfig,
    spill_counter: u64,
}

#[derive(Clone, Debug)]
pub enum StoreError {
    SpillIo(String),
    ReloadIo(String),
    NotFound(FrameId),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::SpillIo(msg) => write!(f, "frame spill I/O error: {msg}"),
            StoreError::ReloadIo(msg) => write!(f, "frame reload I/O error: {msg}"),
            StoreError::NotFound(id) => write!(f, "frame {id} not found in store"),
        }
    }
}

impl std::error::Error for StoreError {}

pub struct FrameStore {
    inner: Mutex<FrameStoreInner>,
}

impl FrameStore {
    pub fn new(spill_config: SpillConfig) -> std::io::Result<Self> {
        std::fs::create_dir_all(&spill_config.spill_dir)?;
        Ok(Self {
            inner: Mutex::new(FrameStoreInner {
                slots: BTreeMap::new(),
                total_in_memory_bytes: 0,
                spill_config,
                spill_counter: 0,
            }),
        })
    }

    /// Add a freshly captured frame to the store. Returns the frame's assigned
    /// id (which is just `frame.id` — the caller is responsible for assigning
    /// monotonic ids).
    pub fn push(&self, frame: Frame) -> FrameId {
        let id = frame.id;
        let heap = frame.approx_heap_bytes();
        let mut inner = self.inner.lock().expect("frame store poisoned");
        inner.total_in_memory_bytes = inner.total_in_memory_bytes.saturating_add(heap);
        inner.slots.insert(id, FrameSlot::InMemory(frame));
        // Spill check happens lazily on `release_before` / `get` to avoid
        // blocking the capture thread on disk I/O. Callers that care about
        // tight memory bounds should call `maybe_spill` periodically.
        id
    }

    /// Fetch a frame by id. If the frame has been spilled to disk, it is
    /// reloaded into memory (and stays in memory until the next spill cycle).
    pub fn get(&self, id: FrameId) -> Result<Frame, StoreError> {
        let needs_reload = {
            let inner = self.inner.lock().expect("frame store poisoned");
            matches!(inner.slots.get(&id), Some(FrameSlot::Spilled(_)))
        };
        if needs_reload {
            self.reload_spilled(id)?;
        }
        let inner = self.inner.lock().expect("frame store poisoned");
        match inner.slots.get(&id) {
            Some(FrameSlot::InMemory(f)) => Ok(f.clone()),
            Some(FrameSlot::Spilled(_)) => Err(StoreError::ReloadIo(format!(
                "frame {id} still spilled after reload attempt"
            ))),
            None => Err(StoreError::NotFound(id)),
        }
    }

    /// Release all frames with id <= `acked`. Called after the cursor registry
    /// has computed the new minimum acked id.
    pub fn release_before(&self, acked: FrameId) {
        let mut inner = self.inner.lock().expect("frame store poisoned");
        let to_remove: Vec<FrameId> = inner
            .slots
            .range(..=acked)
            .map(|(id, _)| *id)
            .collect();
        for id in to_remove {
            if let Some(slot) = inner.slots.remove(&id) {
                match slot {
                    FrameSlot::InMemory(f) => {
                        inner.total_in_memory_bytes =
                            inner.total_in_memory_bytes.saturating_sub(f.approx_heap_bytes());
                    }
                    FrameSlot::Spilled(sp) => {
                        // Best-effort delete of the spill file; ignore errors.
                        let _ = std::fs::remove_file(&sp.spill_path);
                    }
                }
            }
        }
    }

    /// If the in-memory footprint exceeds the configured threshold, spill the
    /// oldest in-memory frames to disk until under threshold. Returns the
    /// number of frames spilled.
    pub fn maybe_spill(&self) -> Result<usize, StoreError> {
        let to_spill: Vec<FrameId> = {
            let inner = self.inner.lock().expect("frame store poisoned");
            if inner.total_in_memory_bytes <= inner.spill_config.max_in_memory_bytes {
                return Ok(0);
            }
            inner
                .slots
                .iter()
                .filter_map(|(id, slot)| match slot {
                    FrameSlot::InMemory(_) => Some(*id),
                    FrameSlot::Spilled(_) => None,
                })
                .take(64) // spill at most 64 frames per call to bound latency
                .collect()
        };

        let mut spilled = 0;
        for id in to_spill {
            // Re-check we still need to spill.
            let still_over = {
                let inner = self.inner.lock().expect("frame store poisoned");
                inner.total_in_memory_bytes > inner.spill_config.max_in_memory_bytes
            };
            if !still_over {
                break;
            }
            self.spill_one(id)?;
            spilled += 1;
        }
        Ok(spilled)
    }

    fn spill_one(&self, id: FrameId) -> Result<(), StoreError> {
        let frame = {
            let mut inner = self.inner.lock().expect("frame store poisoned");
            match inner.slots.remove(&id) {
                Some(FrameSlot::InMemory(f)) => {
                    inner.total_in_memory_bytes =
                        inner.total_in_memory_bytes.saturating_sub(f.approx_heap_bytes());
                    f
                }
                Some(FrameSlot::Spilled(sp)) => {
                    // Already spilled — put it back and return.
                    inner.slots.insert(id, FrameSlot::Spilled(sp));
                    return Ok(());
                }
                None => return Ok(()),
            }
        };

        let mut inner = self.inner.lock().expect("frame store poisoned");
        inner.spill_counter += 1;
        let spill_path = inner
            .spill_config
            .spill_dir
            .join(format!("frame_{id:020}.bin"));
        let spilled = SpilledFrame {
            id: frame.id,
            width: frame.width,
            height: frame.height,
            format: frame.format,
            capture_duration_us: frame.capture_duration_us,
            capture_timestamp_ns: frame.capture_timestamp_ns,
            spill_path: spill_path.clone(),
        };
        drop(inner);

        // Disk write happens outside the lock to avoid blocking other consumers.
        // The slot is marked as Spilled only after the write succeeds; if the
        // write fails, we re-insert the InMemory slot and propagate the error.
        match std::fs::write(&spill_path, frame.data.as_slice()) {
            Ok(()) => {
                let mut inner = self.inner.lock().expect("frame store poisoned");
                inner.slots.insert(id, FrameSlot::Spilled(spilled));
                Ok(())
            }
            Err(error) => {
                let mut inner = self.inner.lock().expect("frame store poisoned");
                inner.total_in_memory_bytes =
                    inner.total_in_memory_bytes.saturating_add(frame.approx_heap_bytes());
                inner.slots.insert(id, FrameSlot::InMemory(frame));
                Err(StoreError::SpillIo(error.to_string()))
            }
        }
    }

    fn reload_spilled(&self, id: FrameId) -> Result<(), StoreError> {
        let spilled = {
            let inner = self.inner.lock().expect("frame store poisoned");
            match inner.slots.get(&id) {
                Some(FrameSlot::Spilled(s)) => s.clone(),
                _ => return Ok(()),
            }
        };

        let frame = spilled.reload().map_err(|e| StoreError::ReloadIo(e.to_string()))?;
        let heap = frame.approx_heap_bytes();

        let mut inner = self.inner.lock().expect("frame store poisoned");
        inner.total_in_memory_bytes = inner.total_in_memory_bytes.saturating_add(heap);
        inner.slots.insert(id, FrameSlot::InMemory(frame));
        Ok(())
    }

    /// Current in-memory footprint in bytes. For diagnostics.
    pub fn in_memory_bytes(&self) -> usize {
        self.inner
            .lock()
            .expect("frame store poisoned")
            .total_in_memory_bytes
    }

    /// Total number of frames currently held (in-memory + spilled).
    pub fn len(&self) -> usize {
        self.inner.lock().expect("frame store poisoned").slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
