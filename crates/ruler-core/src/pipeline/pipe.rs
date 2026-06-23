//! Windows named pipe transport for the capture pipeline.
//!
//! The pipeline exposes a single named pipe endpoint
//! (`\\.\pipe\ruler-frames-{session}`) that any number of consumers can
//! connect to — both in-process (the analysis layer, the debug recorder, the
//! calibration loop) and external (future plugin processes).
//!
//! Wire protocol
//! -------------
//! All integers are little-endian. Magic values are 4-byte ASCII tags that
//! let consumers sanity-check stream alignment. A reference Python decoder is
//! sketched in `docs/API.md`.
//!
//! **Subscribe** (client → server, sent once on connect):
//! ```text
//! [4]  magic = b"RSB1"
//! [1]  policy (0 = InOrder, 1 = SkipToLatest)
//! [8]  start_frame_id (u64) — consumer's initial cursor
//! ```
//!
//! **Frame** (server → client):
//! ```text
//! [4]  magic = b"RFM1"
//! [8]  frame_id (u64)
//! [4]  width (u32)
//! [4]  height (u32)
//! [4]  format_tag (u32) — see pixel_format_tag
//! [8]  capture_duration_us (u64)
//! [8]  capture_timestamp_ns (u64)
//! [8]  data_len (u64)
//! [data_len] pixel bytes
//! ```
//!
//! **Pull** (client → server, SkipToLatest only):
//! ```text
//! [4]  magic = b"RPUL"
//! ```
//! The server responds with the latest frame whose id is greater than the
//! consumer's last-delivered id. If no such frame exists, the server blocks
//! until one arrives (or until shutdown, in which case it closes the
//! connection).
//!
//! **Ack** (client → server, InOrder only):
//! ```text
//! [4]  magic = b"RACK"
//! [8]  frame_id (u64) — last fully-processed frame
//! ```
//!
//! **Error** (server → client):
//! ```text
//! [4]  magic = b"RERR"
//! [2]  message_len (u16)
//! [message_len] UTF-8 message
//! ```

#![cfg(windows)]

use std::ffi::c_void;
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Threading::INFINITE;

use super::cursor::ConsumerPolicy;
use super::frame::{pixel_format_from_tag, pixel_format_tag, Frame, FrameId};

// ---------------------------------------------------------------------------
// Raw FFI for the named-pipe and file I/O APIs. The `windows` crate's
// high-level bindings for `CreateFileW`/`ReadFile`/`WriteFile` require
// feature flags that are not enabled in the workspace Cargo.toml; we follow
// the same `extern "system"` pattern used by `capture/windows.rs` to avoid
// adding more features.
// ---------------------------------------------------------------------------

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
const PIPE_READMODE_BYTE: u32 = 0x0000_0000;
const PIPE_WAIT: u32 = 0x0000_0000;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;
const PIPE_UNLIMITED_INSTANCES: u32 = 255;

const ERROR_PIPE_CONNECTED: u32 = 535;

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        lp_file_name: *const u16,
        dw_desired_access: u32,
        dw_share_mode: u32,
        lp_security_attributes: *const c_void,
        dw_creation_disposition: u32,
        dw_flags_and_attributes: u32,
        h_template_file: *const c_void,
    ) -> HANDLE;

    fn CreateNamedPipeW(
        lp_name: *const u16,
        dw_open_mode: u32,
        dw_pipe_mode: u32,
        n_max_instances: u32,
        n_out_buffer_size: u32,
        n_in_buffer_size: u32,
        n_default_time_out: u32,
        lp_security_attributes: *const c_void,
    ) -> HANDLE;

    fn ConnectNamedPipe(
        h_named_pipe: HANDLE,
        lp_overlapped: *const c_void,
    ) -> i32;

    fn ReadFile(
        h_file: HANDLE,
        lp_buffer: *mut u8,
        n_number_of_bytes_to_read: u32,
        lp_number_of_bytes_read: *mut u32,
        lp_overlapped: *const c_void,
    ) -> i32;

    fn WriteFile(
        h_file: HANDLE,
        lp_buffer: *const u8,
        n_number_of_bytes_to_write: u32,
        lp_number_of_bytes_written: *mut u32,
        lp_overlapped: *const c_void,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLastError() -> u32;
}

/// Magic values for the wire protocol. Public so external consumers (e.g. a
/// Python plugin) can re-implement the protocol without depending on
/// ruler-core.
pub mod protocol {
    pub const SUBSCRIBE_MAGIC: [u8; 4] = *b"RSB1";
    pub const FRAME_MAGIC: [u8; 4] = *b"RFM1";
    pub const PULL_MAGIC: [u8; 4] = *b"RPUL";
    pub const ACK_MAGIC: [u8; 4] = *b"RACK";
    pub const NO_FRAME_MAGIC: [u8; 4] = *b"RNON";
    pub const ERROR_MAGIC: [u8; 4] = *b"RERR";

    pub const POLICY_IN_ORDER: u8 = 0;
    pub const POLICY_SKIP_TO_LATEST: u8 = 1;
}

/// Errors that can occur on the pipe transport.
#[derive(Debug)]
pub enum PipeError {
    /// Underlying I/O error (broken pipe, read/write failure).
    Io(io::Error),
    /// Peer sent a malformed packet (bad magic, truncated body).
    Protocol(String),
    /// Server explicitly sent an error frame.
    Server(String),
    /// Pipeline is shutting down; no more frames will be delivered.
    Shutdown,
}

impl std::fmt::Display for PipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipeError::Io(e) => write!(f, "pipe I/O error: {e}"),
            PipeError::Protocol(msg) => write!(f, "pipe protocol error: {msg}"),
            PipeError::Server(msg) => write!(f, "pipe server error: {msg}"),
            PipeError::Shutdown => write!(f, "pipe shutting down"),
        }
    }
}

impl std::error::Error for PipeError {}

impl From<io::Error> for PipeError {
    fn from(value: io::Error) -> Self {
        PipeError::Io(value)
    }
}

/// Convert a Rust string to a NUL-terminated UTF-16 vector for use with
/// `*W` Windows APIs.
fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Thin wrapper around a Windows `HANDLE` that implements `Read`+`Write` and
/// closes the handle on drop.
pub struct PipeHandle(HANDLE);

// SAFETY: Handles returned by CreateNamedPipeW / CreateFileW are safe to send
// across threads. Synchronous access is the caller's responsibility.
unsafe impl Send for PipeHandle {}
unsafe impl Sync for PipeHandle {}

impl PipeHandle {
    /// Take ownership of a raw handle. The handle will be closed on drop.
    ///
    /// # Safety
    /// The caller must ensure the handle is valid and not owned by anyone
    /// else.
    pub unsafe fn from_raw(handle: HANDLE) -> Self {
        Self(handle)
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for PipeHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() && self.0 != INVALID_HANDLE_VALUE {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

impl Read for PipeHandle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut bytes_read: u32 = 0;
        let ok = unsafe {
            ReadFile(
                self.0,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut bytes_read as *mut u32,
                std::ptr::null(),
            )
        };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            return Err(io::Error::other(format!("ReadFile failed (GetLastError={code})")));
        }
        if bytes_read == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "pipe closed"));
        }
        Ok(bytes_read as usize)
    }
}

impl Write for PipeHandle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut bytes_written: u32 = 0;
        let ok = unsafe {
            WriteFile(
                self.0,
                buf.as_ptr(),
                buf.len() as u32,
                &mut bytes_written as *mut u32,
                std::ptr::null(),
            )
        };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            return Err(io::Error::other(format!("WriteFile failed (GetLastError={code})")));
        }
        Ok(bytes_written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        // Named pipes are unbuffered at the Rust level; nothing to flush.
        Ok(())
    }
}

fn read_exact<R: Read>(r: &mut R, n: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

fn read_u16_le(r: &mut impl Read) -> io::Result<u16> {
    let b = read_exact(r, 2)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn read_u32_le(r: &mut impl Read) -> io::Result<u32> {
    let b = read_exact(r, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_u64_le(r: &mut impl Read) -> io::Result<u64> {
    let b = read_exact(r, 8)?;
    Ok(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

fn write_u16_le(w: &mut impl Write, v: u16) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn write_u32_le(w: &mut impl Write, v: u32) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn write_u64_le(w: &mut impl Write, v: u64) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

/// Server-side endpoint. Owns the pipe name and accepts incoming consumer
/// connections in a blocking manner.
pub struct PipeServer {
    pipe_name: String,
    shutdown: AtomicBool,
}

impl PipeServer {
    /// Create the named pipe endpoint. Does not yet accept connections; call
    /// [`PipeServer::accept`] in a loop to handle incoming consumers.
    pub fn new(session_id: &str) -> io::Result<Self> {
        Ok(Self {
            pipe_name: format!(r"\\.\pipe\ruler-frames-{session_id}"),
            shutdown: AtomicBool::new(false),
        })
    }

    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    /// Signal the accept loop to stop. The next [`PipeServer::accept`] call
    /// will return `Err(PipeError::Shutdown)`. This works by connecting to
    /// our own pipe name to unblock any pending `ConnectNamedPipe`.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Self-connect to unblock a pending accept.
        let wide_name = wide(&self.pipe_name);
        unsafe {
            let _ = CreateFileW(
                wide_name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null(),
            );
        }
    }

    /// Block until a new consumer connects, then return the connected handle
    /// plus the consumer's subscribe packet. Returns `Err(Shutdown)` if the
    /// server is shutting down.
    ///
    /// This creates a fresh pipe instance for each accept call. Windows named
    /// pipe semantics allow multiple concurrent instances of the same name.
    pub fn accept(&self) -> Result<(PipeHandle, ConsumerPolicy, FrameId), PipeError> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(PipeError::Shutdown);
        }

        let wide_name = wide(&self.pipe_name);
        let handle = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                // 4 MiB buffers — enough for one 1440p RGBA frame plus header.
                4 << 20,
                4 << 20,
                0,
                std::ptr::null(),
            )
        };
        if handle.is_invalid() || handle == INVALID_HANDLE_VALUE {
            let code = unsafe { GetLastError() };
            return Err(PipeError::Io(io::Error::other(format!(
                "CreateNamedPipeW failed (GetLastError={code})"
            ))));
        }

        // Block until a client connects. ConnectNamedPipe returns 0 on failure
        // — but if a client connects between CreateNamedPipeW and
        // ConnectNamedPipe, GetLastError will be ERROR_PIPE_CONNECTED, which
        // is not actually an error.
        let connect_result = unsafe { ConnectNamedPipe(handle, std::ptr::null()) };
        if connect_result == 0 {
            let code = unsafe { GetLastError() };
            if code != ERROR_PIPE_CONNECTED {
                let _ = unsafe { CloseHandle(handle) };
                if self.shutdown.load(Ordering::SeqCst) {
                    return Err(PipeError::Shutdown);
                }
                return Err(PipeError::Io(io::Error::other(format!(
                    "ConnectNamedPipe failed (GetLastError={code})"
                ))));
            }
        }

        // If we shut down between the connect succeeding and now, close the
        // (likely self-connected) handle and return Shutdown.
        if self.shutdown.load(Ordering::SeqCst) {
            let _ = unsafe { CloseHandle(handle) };
            return Err(PipeError::Shutdown);
        }

        let mut pipe = unsafe { PipeHandle::from_raw(handle) };
        let (policy, start_frame_id) = read_subscribe(&mut pipe)?;
        Ok((pipe, policy, start_frame_id))
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Read the subscribe packet from a freshly-connected consumer.
fn read_subscribe(pipe: &mut PipeHandle) -> Result<(ConsumerPolicy, FrameId), PipeError> {
    let magic = read_exact(pipe, 4)?;
    if magic != protocol::SUBSCRIBE_MAGIC {
        return Err(PipeError::Protocol(format!("bad subscribe magic: {:?}", magic)));
    }
    let policy_byte = read_exact(pipe, 1)?;
    let policy = match policy_byte[0] {
        protocol::POLICY_IN_ORDER => ConsumerPolicy::InOrder,
        protocol::POLICY_SKIP_TO_LATEST => ConsumerPolicy::SkipToLatest,
        other => return Err(PipeError::Protocol(format!("unknown policy byte: {other}"))),
    };
    let start_frame_id = read_u64_le(pipe)?;
    Ok((policy, start_frame_id))
}

/// Write a frame packet to a pipe. Used by the server's per-consumer sender
/// thread.
pub fn write_frame(pipe: &mut PipeHandle, frame: &Frame) -> io::Result<()> {
    pipe.write_all(&protocol::FRAME_MAGIC)?;
    write_u64_le(pipe, frame.id)?;
    write_u32_le(pipe, frame.width)?;
    write_u32_le(pipe, frame.height)?;
    write_u32_le(pipe, pixel_format_tag(frame.format))?;
    write_u64_le(pipe, frame.capture_duration_us)?;
    write_u64_le(pipe, frame.capture_timestamp_ns)?;
    write_u64_le(pipe, frame.data.len() as u64)?;
    pipe.write_all(frame.data.as_slice())?;
    Ok(())
}

/// Write a "no new frame" packet (SkipToLatest only, used during shutdown).
pub fn write_no_frame(pipe: &mut PipeHandle) -> io::Result<()> {
    pipe.write_all(&protocol::NO_FRAME_MAGIC)
}

/// Write an error packet and flush. Used by the server when it needs to
/// terminate a consumer connection gracefully.
pub fn write_error(pipe: &mut PipeHandle, message: &str) -> io::Result<()> {
    pipe.write_all(&protocol::ERROR_MAGIC)?;
    let bytes = message.as_bytes();
    let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
    write_u16_le(pipe, len)?;
    pipe.write_all(&bytes[..len as usize])?;
    Ok(())
}

/// Read an ack packet (InOrder only). Returns the acked frame id.
pub fn read_ack(pipe: &mut PipeHandle) -> Result<FrameId, PipeError> {
    let magic = read_exact(pipe, 4)?;
    if magic != protocol::ACK_MAGIC {
        return Err(PipeError::Protocol(format!("expected RACK, got {:?}", magic)));
    }
    let frame_id = read_u64_le(pipe)?;
    Ok(frame_id)
}

/// Read a pull packet (SkipToLatest only).
pub fn read_pull(pipe: &mut PipeHandle) -> Result<(), PipeError> {
    let magic = read_exact(pipe, 4)?;
    if magic != protocol::PULL_MAGIC {
        return Err(PipeError::Protocol(format!("expected RPUL, got {:?}", magic)));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Client side — used by in-process consumers (and as a reference for external
// consumers that want to speak the protocol directly).
// ---------------------------------------------------------------------------

/// Client-side handle to the capture pipeline. Created by
/// [`super::CapturePipeline::connect_consumer`] for in-process consumers.
pub struct ConsumerPipe {
    handle: PipeHandle,
    policy: ConsumerPolicy,
}

impl ConsumerPipe {
    /// Open a connection to the named pipe server. Sends the subscribe
    /// packet with the given policy and start cursor.
    pub fn connect(pipe_name: &str, policy: ConsumerPolicy, start_frame_id: FrameId) -> io::Result<Self> {
        let wide_name = wide(pipe_name);
        let handle = unsafe {
            CreateFileW(
                wide_name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null(),
            )
        };
        if handle.is_invalid() || handle == INVALID_HANDLE_VALUE {
            let code = unsafe { GetLastError() };
            return Err(io::Error::other(format!(
                "CreateFileW for pipe failed (GetLastError={code})"
            )));
        }

        let mut pipe = unsafe { PipeHandle::from_raw(handle) };
        // Send subscribe packet.
        pipe.write_all(&protocol::SUBSCRIBE_MAGIC)?;
        let policy_byte = match policy {
            ConsumerPolicy::InOrder => protocol::POLICY_IN_ORDER,
            ConsumerPolicy::SkipToLatest => protocol::POLICY_SKIP_TO_LATEST,
        };
        pipe.write_all(&[policy_byte])?;
        write_u64_le(&mut pipe, start_frame_id)?;

        Ok(Self {
            handle: pipe,
            policy,
        })
    }

    pub fn policy(&self) -> ConsumerPolicy {
        self.policy
    }

    pub fn handle(&mut self) -> &mut PipeHandle {
        &mut self.handle
    }

    /// Receive the next frame from the pipeline.
    ///
    /// For `InOrder` consumers this blocks until the next frame is pushed by
    /// the server. For `SkipToLatest` consumers this sends a `RPUL` request
    /// and blocks until the server responds with a frame (the server blocks
    /// internally until a frame newer than the consumer's cursor exists).
    pub fn recv_frame(&mut self) -> Result<Frame, PipeError> {
        if self.policy == ConsumerPolicy::SkipToLatest {
            self.handle.write_all(&protocol::PULL_MAGIC)?;
        }
        let magic = read_exact(&mut self.handle, 4)?;
        match magic.as_slice() {
            b"RFM1" => {
                let id = read_u64_le(&mut self.handle)?;
                let width = read_u32_le(&mut self.handle)?;
                let height = read_u32_le(&mut self.handle)?;
                let format_tag = read_u32_le(&mut self.handle)?;
                let capture_duration_us = read_u64_le(&mut self.handle)?;
                let capture_timestamp_ns = read_u64_le(&mut self.handle)?;
                let data_len = read_u64_le(&mut self.handle)? as usize;
                let format = pixel_format_from_tag(format_tag)
                    .ok_or_else(|| PipeError::Protocol(format!("unknown format tag {format_tag}")))?;
                let mut data = vec![0u8; data_len];
                self.handle.read_exact(&mut data)?;
                Ok(Frame {
                    id,
                    width,
                    height,
                    format,
                    capture_duration_us,
                    capture_timestamp_ns,
                    data: std::sync::Arc::new(data),
                })
            }
            b"RNON" => {
                // For SkipToLatest, the server only sends RFM1 (it blocks
                // until a frame is available). RNON is only used during
                // shutdown — treat it as shutdown.
                Err(PipeError::Shutdown)
            }
            b"RERR" => {
                let len = read_u16_le(&mut self.handle)? as usize;
                let msg = read_exact(&mut self.handle, len)?;
                let msg = String::from_utf8_lossy(&msg).into_owned();
                Err(PipeError::Server(msg))
            }
            other => Err(PipeError::Protocol(format!(
                "expected RFM1/RNON/RERR, got {:?}",
                other
            ))),
        }
    }

    /// Acknowledge a frame (InOrder only). For SkipToLatest this is a no-op.
    pub fn ack(&mut self, frame_id: FrameId) -> Result<(), PipeError> {
        if self.policy == ConsumerPolicy::SkipToLatest {
            return Ok(());
        }
        self.handle.write_all(&protocol::ACK_MAGIC)?;
        write_u64_le(&mut self.handle, frame_id)?;
        Ok(())
    }
}

// Suppress unused-import warning for INFINITE (kept for future timeout work).
#[allow(dead_code)]
const _INFINITE: u32 = INFINITE;

// Suppress unused-import warning for PCWSTR (kept for potential API migration).
#[allow(dead_code)]
fn _pcwstr_marker(_: PCWSTR) {}
