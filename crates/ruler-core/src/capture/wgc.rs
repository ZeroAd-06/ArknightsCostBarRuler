//! Windows Graphics Capture (WGC) + D3D11 backend for fast PC window capture.
//!
//! This replaces the GDI `PrintWindow` path (`windows.rs`) for the PC
//! Arknights client. The DWM hands us the window's already-composited texture
//! on the GPU (zero CPU re-rasterization), and we do a single GPU→CPU staging
//! readback per frame. Measured latency is comparable to the MuMu shared-memory
//! IPC path (~5ms), versus 15–50ms for `PrintWindow` on a DirectX-rendered
//! window.
//!
//! Threading / apartment model
//! ---------------------------
//! `CapturePipeline::start` calls `connect()` on its own thread, but
//! `capture_frame()`/`disconnect()` run on the dedicated capture thread. WGC is
//! WinRT and `RoInitialize` is per-thread, so all D3D/WGC objects are created
//! lazily on the *capture* thread (first `capture_frame`). `connect()` only does
//! a lightweight pre-check (locate HWND, read client size, `IsSupported`), so a
//! failure there lets `create_backend` fall back to the GDI backend.
//!
//! Frame delivery uses the free-threaded frame pool: `FrameArrived` fires on the
//! pool's worker thread and signals a condvar; `capture_frame` blocks on it,
//! then drains `TryGetNextFrame` to the latest frame (skip-to-latest, matching
//! the real-time ruler's needs).

use std::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use windows::core::{IInspectable, Interface};
use windows::Foundation::{EventRegistrationToken, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{BOOL, HMODULE, HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

use crate::analysis::scanner::PixelFormat;
use crate::capture::{CaptureBackend, CapturedFrame, WindowInfo};

// Raw user32 entry points for cursor-occlusion geometry. Mirrors the bare-FFI
// style of `windows.rs` so we do not depend on the high-level binding module
// paths for these few calls.
#[link(name = "user32")]
unsafe extern "system" {
    fn GetClientRect(hwnd: HWND, lp_rect: *mut RECT) -> BOOL;
    fn ClientToScreen(hwnd: HWND, lp_point: *mut POINT) -> BOOL;
    fn IsWindow(hwnd: HWND) -> BOOL;
}

/// Number of frame-pool buffers. Two lets a new frame arrive while we still hold
/// the previous one, without unbounded queueing.
const FRAME_POOL_BUFFERS: i32 = 2;
/// WGC output format: BGRA8 (matches `PixelFormat::Bgra`).
const WGC_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
/// How long `capture_frame` waits for the next `FrameArrived` before giving up.
const FRAME_TIMEOUT: Duration = Duration::from_secs(1);

pub struct WgcController {
    /// Original handle from config, used to (re-)locate the window in `connect`.
    handle: Option<isize>,
    title: Option<String>,
    class: Option<String>,
    /// Resolved target window. Set in `connect`.
    hwnd: Option<HWND>,
    width: u32,
    height: u32,

    // Lazily-created on the capture thread (first `capture_frame`).
    ro_initialized: bool,
    device: Option<ID3D11Device>,
    context: Option<ID3D11DeviceContext>,
    d3d_device: Option<IDirect3DDevice>,
    item: Option<GraphicsCaptureItem>,
    frame_pool: Option<Direct3D11CaptureFramePool>,
    session: Option<GraphicsCaptureSession>,
    staging: Option<ID3D11Texture2D>,
    frame_token: Option<EventRegistrationToken>,

    /// Signalled by the `FrameArrived` handler (pool worker thread); waited on by
    /// `capture_frame`.
    frame_signal: Arc<(Mutex<bool>, Condvar)>,
}

// SAFETY: the COM/WinRT objects are only ever touched on the capture thread.
// The controller is constructed (with all COM fields `None`) on the pipeline's
// start thread and moved once into the capture thread, where lazy init fills
// them in. This mirrors `WindowsController`'s `unsafe impl Send`.
unsafe impl Send for WgcController {}

impl WgcController {
    pub fn new(
        window_handle: Option<isize>,
        window_title: Option<String>,
        window_class: Option<String>,
    ) -> Self {
        Self {
            handle: window_handle,
            title: window_title,
            class: window_class,
            hwnd: window_handle.map(|value| HWND(value as *mut c_void)),
            width: 0,
            height: 0,
            ro_initialized: false,
            device: None,
            context: None,
            d3d_device: None,
            item: None,
            frame_pool: None,
            session: None,
            staging: None,
            frame_token: None,
            frame_signal: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    /// Create the D3D/WGC objects on the current (capture) thread. Idempotent
    /// guard is the caller's `self.frame_pool.is_none()` check.
    fn lazy_init(&mut self) -> Result<(), String> {
        let hwnd = self
            .hwnd
            .ok_or_else(|| "WGC backend is not connected".to_string())?;

        if !self.ro_initialized {
            // Ignore "already initialized" / "changed mode" — another component
            // may have initialized COM on this thread already.
            let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
            self.ro_initialized = true;
        }

        let (device, context) = create_d3d_device()?;

        let dxgi: IDXGIDevice = device.cast().map_err(|e| format!("DXGI device cast: {e}"))?;
        let inspectable = unsafe {
            CreateDirect3D11DeviceFromDXGIDevice(&dxgi)
                .map_err(|e| format!("CreateDirect3D11DeviceFromDXGIDevice: {e}"))?
        };
        let d3d_device: IDirect3DDevice = inspectable
            .cast()
            .map_err(|e| format!("IDirect3DDevice cast: {e}"))?;

        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| format!("GraphicsCaptureItem interop factory: {e}"))?;
        let item: GraphicsCaptureItem = unsafe {
            interop
                .CreateForWindow(hwnd)
                .map_err(|e| format!("CreateForWindow: {e}"))?
        };

        let size = item.Size().map_err(|e| format!("item.Size: {e}"))?;
        self.width = size.Width.max(0) as u32;
        self.height = size.Height.max(0) as u32;

        let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &d3d_device,
            WGC_FORMAT,
            FRAME_POOL_BUFFERS,
            size,
        )
        .map_err(|e| format!("CreateFreeThreaded: {e}"))?;

        let session = frame_pool
            .CreateCaptureSession(&item)
            .map_err(|e| format!("CreateCaptureSession: {e}"))?;
        // Best-effort: hide the system cursor (the app draws its own) and the
        // yellow capture border. Both can fail on older Windows; ignore.
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetIsBorderRequired(false);

        let signal = Arc::clone(&self.frame_signal);
        let handler = TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(
            move |_pool, _args| {
                let (lock, cvar) = &*signal;
                if let Ok(mut ready) = lock.lock() {
                    *ready = true;
                    cvar.notify_one();
                }
                Ok(())
            },
        );
        let frame_token = frame_pool
            .FrameArrived(&handler)
            .map_err(|e| format!("FrameArrived subscribe: {e}"))?;

        session
            .StartCapture()
            .map_err(|e| format!("StartCapture: {e}"))?;

        let staging = create_staging(&device, self.width, self.height)?;

        self.device = Some(device);
        self.context = Some(context);
        self.d3d_device = Some(d3d_device);
        self.item = Some(item);
        self.frame_pool = Some(frame_pool);
        self.session = Some(session);
        self.staging = Some(staging);
        self.frame_token = Some(frame_token);
        Ok(())
    }

    /// Wait for the next `FrameArrived` signal. Returns `false` on timeout.
    fn wait_for_frame(&self) -> bool {
        let (lock, cvar) = &*self.frame_signal;
        let mut ready = match lock.lock() {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        if !*ready {
            let (guard, timeout) = match cvar.wait_timeout(ready, FRAME_TIMEOUT) {
                Ok(pair) => pair,
                Err(_) => return false,
            };
            ready = guard;
            if timeout.timed_out() && !*ready {
                return false;
            }
        }
        *ready = false;
        true
    }

    /// Recreate the frame pool and staging texture for a new content size.
    fn recreate(&mut self, size: SizeInt32) -> Result<(), String> {
        self.width = size.Width.max(0) as u32;
        self.height = size.Height.max(0) as u32;
        let d3d_device = self
            .d3d_device
            .as_ref()
            .ok_or_else(|| "WGC device missing on recreate".to_string())?;
        if let Some(pool) = &self.frame_pool {
            pool.Recreate(d3d_device, WGC_FORMAT, FRAME_POOL_BUFFERS, size)
                .map_err(|e| format!("frame pool Recreate: {e}"))?;
        }
        let device = self
            .device
            .as_ref()
            .ok_or_else(|| "WGC D3D device missing on recreate".to_string())?;
        self.staging = Some(create_staging(device, self.width, self.height)?);
        Ok(())
    }
}

impl CaptureBackend for WgcController {
    fn connect(&mut self) -> Result<(), String> {
        // Lightweight pre-check only — no D3D/WGC objects here (see module docs).
        let hwnd = super::window_find::locate_target_window(self.handle, &self.title, &self.class)?;
        // WinRT must be initialized on this thread before activating the WGC
        // factory behind `IsSupported`. Harmless if already initialized (any
        // apartment); the capture thread re-initializes itself in `lazy_init`.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        if !GraphicsCaptureSession::IsSupported().map_err(|e| format!("WGC IsSupported: {e}"))? {
            return Err("Windows Graphics Capture is not supported on this system".to_string());
        }
        let (width, height) = client_size(hwnd)?;
        self.hwnd = Some(hwnd);
        self.width = width;
        self.height = height;
        Ok(())
    }

    fn capture_frame(&mut self) -> Result<CapturedFrame, String> {
        if self.frame_pool.is_none() {
            self.lazy_init()?;
        }

        if !self.wait_for_frame() {
            return Err("WGC: no frame arrived within timeout".to_string());
        }

        // Drain to the latest available frame; close the ones we skip. The
        // bounded loop is a safety net against an unexpected non-erroring null.
        let frame: Direct3D11CaptureFrame = {
            let pool = self
                .frame_pool
                .as_ref()
                .ok_or_else(|| "WGC frame pool missing".to_string())?;
            let mut latest: Option<Direct3D11CaptureFrame> = None;
            for _ in 0..FRAME_POOL_BUFFERS.max(1) + 2 {
                match pool.TryGetNextFrame() {
                    Ok(next) => {
                        if let Some(prev) = latest.replace(next) {
                            let _ = prev.Close();
                        }
                    }
                    Err(_) => break,
                }
            }
            latest.ok_or_else(|| "WGC: signalled but no frame available".to_string())?
        };

        // Handle a window resize: rebuild the pool/staging and skip this frame.
        let size = frame.ContentSize().map_err(|e| format!("frame.ContentSize: {e}"))?;
        if size.Width.max(0) as u32 != self.width || size.Height.max(0) as u32 != self.height {
            let _ = frame.Close();
            self.recreate(size)?;
            return Err("WGC: target resized; rebuilding capture".to_string());
        }

        let surface = frame.Surface().map_err(|e| format!("frame.Surface: {e}"))?;
        let access: IDirect3DDxgiInterfaceAccess = surface
            .cast()
            .map_err(|e| format!("surface DXGI access cast: {e}"))?;
        let texture: ID3D11Texture2D = unsafe {
            access
                .GetInterface()
                .map_err(|e| format!("surface GetInterface<ID3D11Texture2D>: {e}"))?
        };

        let context = self
            .context
            .as_ref()
            .ok_or_else(|| "WGC device context missing".to_string())?;
        let staging = self
            .staging
            .as_ref()
            .ok_or_else(|| "WGC staging texture missing".to_string())?;

        let width = self.width as usize;
        let height = self.height as usize;
        let dst_stride = width * 4;
        let data = unsafe {
            context.CopyResource(staging, &texture);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| format!("staging Map: {e}"))?;

            let mut data = vec![0u8; dst_stride * height];
            let src_base = mapped.pData as *const u8;
            let src_pitch = mapped.RowPitch as usize;
            // Single pass: handle the staging RowPitch AND flip top-down → the
            // bottom-up order the scanner expects, at zero extra cost.
            for y in 0..height {
                let src = src_base.add(y * src_pitch);
                let dst_row = (height - 1 - y) * dst_stride;
                std::ptr::copy_nonoverlapping(src, data.as_mut_ptr().add(dst_row), dst_stride);
            }
            context.Unmap(staging, 0);
            data
        };

        let _ = frame.Close();

        Ok(CapturedFrame {
            data,
            width: self.width,
            height: self.height,
            format: PixelFormat::Bgra,
        })
    }

    fn disconnect(&mut self) {
        if let (Some(pool), Some(token)) = (self.frame_pool.as_ref(), self.frame_token) {
            let _ = pool.RemoveFrameArrived(token);
        }
        if let Some(session) = self.session.take() {
            let _ = session.Close();
        }
        if let Some(pool) = self.frame_pool.take() {
            let _ = pool.Close();
        }
        self.item = None;
        self.staging = None;
        self.context = None;
        self.d3d_device = None;
        self.device = None;
        self.frame_token = None;
        if let Ok(mut ready) = self.frame_signal.0.lock() {
            *ready = false;
        }
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn window_info(&self) -> Option<WindowInfo> {
        let hwnd = self.hwnd?;
        if !unsafe { IsWindow(hwnd) }.as_bool() {
            return None;
        }
        // Re-read live geometry every frame: the window can move after connect,
        // and cursor-occlusion math must use the current screen position.
        let mut rect = RECT::default();
        if !unsafe { GetClientRect(hwnd, &mut rect) }.as_bool() {
            return None;
        }
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return None;
        }
        let mut origin = POINT { x: 0, y: 0 };
        if !unsafe { ClientToScreen(hwnd, &mut origin) }.as_bool() {
            return None;
        }
        Some(WindowInfo {
            hwnd: hwnd.0 as isize,
            client_left: origin.x,
            client_top: origin.y,
            width: width as u32,
            height: height as u32,
        })
    }
}

impl Drop for WgcController {
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// Create a hardware D3D11 device + immediate context with BGRA support
/// (required for WGC interop).
fn create_d3d_device() -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
    }
    let device = device.ok_or_else(|| "D3D11CreateDevice returned no device".to_string())?;
    let context = context.ok_or_else(|| "D3D11CreateDevice returned no context".to_string())?;
    Ok((device, context))
}

/// Create a CPU-readable staging texture matching the capture format/size.
fn create_staging(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut texture: Option<ID3D11Texture2D> = None;
    unsafe {
        device
            .CreateTexture2D(&desc, None, Some(&mut texture))
            .map_err(|e| format!("CreateTexture2D (staging): {e}"))?;
    }
    texture.ok_or_else(|| "CreateTexture2D returned no staging texture".to_string())
}

/// Read the client-area size of a window via `GetClientRect`.
fn client_size(hwnd: HWND) -> Result<(u32, u32), String> {
    let mut rect = RECT::default();
    if !unsafe { GetClientRect(hwnd, &mut rect) }.as_bool() {
        return Err("GetClientRect failed".to_string());
    }
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        return Err(format!("Invalid target client size: {width}x{height}"));
    }
    Ok((width as u32, height as u32))
}
