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
//! Crucially, both paths first call [`ensure_process_mta`] to pin a
//! process-lifetime MTA. windows-rs caches WGC activation factories
//! process-wide, and that cache only stays valid while `GraphicsCapture.dll`
//! remains loaded — which requires the MTA to outlive *every* thread that ever
//! touched WGC, not just the current one. Relying on per-thread `RoInitialize`
//! alone let a short-lived probe thread's exit unload the DLL and dangle the
//! cache (see [`ensure_process_mta`] for the full failure mode).
//!
//! Frame delivery uses the free-threaded frame pool: `FrameArrived` fires on the
//! pool's worker thread and signals a condvar; `capture_frame` blocks on it,
//! then drains `TryGetNextFrame` to the latest frame (skip-to-latest, matching
//! the real-time ruler's needs).

use std::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex, Once};
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
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::Com::CoIncrementMTAUsage;
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
    /// Output (client-area) size. Fixed at `connect`, mirrors what the GDI
    /// backend produces and what `PipelineInfo` broadcasts; every frame is
    /// cropped/emitted at exactly this size so downstream buffers never shift.
    width: u32,
    height: u32,
    /// Frame-pool / capture-texture size = the full DWM-composited window
    /// (extended frame bounds, incl. title bar + borders). WGC always delivers
    /// this; used for resize detection against `frame.ContentSize()`.
    pool_width: u32,
    pool_height: u32,
    /// Top-left of the client area inside the capture texture (texture origin =
    /// extended-frame-bounds origin). Computed once in `lazy_init` from
    /// `DWMWA_EXTENDED_FRAME_BOUNDS`; the crop loop copies the client rectangle.
    crop_x: u32,
    crop_y: u32,

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
            pool_width: 0,
            pool_height: 0,
            crop_x: 0,
            crop_y: 0,
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

        // Guarantee the process MTA is pinned before any WGC object is created,
        // independent of which thread reached `connect` first (see
        // `ensure_process_mta`). The explicit per-thread init below then joins
        // this long-lived capture thread to that MTA.
        ensure_process_mta();
        if !self.ro_initialized {
            // Ignore "already initialized" / "changed mode" — another component
            // may have initialized COM on this thread already.
            let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
            self.ro_initialized = true;
        }

        let (device, context) = create_d3d_device()?;

        let dxgi: IDXGIDevice = device
            .cast()
            .map_err(|e| format!("DXGI device cast: {e}"))?;
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

        // The capture texture spans the whole composited window (title bar +
        // borders). Keep `self.width/height` as the client-area output size
        // (set in `connect`); record the texture size separately for the pool.
        let size = item.Size().map_err(|e| format!("item.Size: {e}"))?;
        self.pool_width = size.Width.max(0) as u32;
        self.pool_height = size.Height.max(0) as u32;
        let (crop_x, crop_y) = client_crop_offset(hwnd, self.pool_width, self.pool_height)?;
        self.crop_x = crop_x;
        self.crop_y = crop_y;

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

        let staging = create_staging(&device, self.pool_width, self.pool_height)?;

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
    /// Only the capture texture (pool) follows the window; the client-area
    /// output size stays fixed (a genuine resize is handled by a reconnect at
    /// the layer above, exactly as the GDI backend requires).
    fn recreate(&mut self, size: SizeInt32) -> Result<(), String> {
        self.pool_width = size.Width.max(0) as u32;
        self.pool_height = size.Height.max(0) as u32;
        let hwnd = self
            .hwnd
            .ok_or_else(|| "WGC backend is not connected".to_string())?;
        let (crop_x, crop_y) = client_crop_offset(hwnd, self.pool_width, self.pool_height)?;
        self.crop_x = crop_x;
        self.crop_y = crop_y;
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
        self.staging = Some(create_staging(device, self.pool_width, self.pool_height)?);
        Ok(())
    }
}

impl CaptureBackend for WgcController {
    fn connect(&mut self) -> Result<(), String> {
        // Lightweight pre-check only — no D3D/WGC objects here (see module docs).
        let hwnd = super::window_find::locate_target_window(self.handle, &self.title, &self.class)?;
        // Pin a process-lifetime MTA before touching the WGC activation factory.
        // A bare per-thread `RoInitialize` is not enough: this `connect` can run
        // on a short-lived probe thread whose exit would tear the MTA down and
        // unload `GraphicsCapture.dll`, dangling the process-wide factory cache.
        // See `ensure_process_mta`.
        ensure_process_mta();
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
        // Compare against the capture-texture (pool) size, not the client size.
        let size = frame
            .ContentSize()
            .map_err(|e| format!("frame.ContentSize: {e}"))?;
        if size.Width.max(0) as u32 != self.pool_width
            || size.Height.max(0) as u32 != self.pool_height
        {
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

        let out_w = self.width as usize;
        let out_h = self.height as usize;
        let out_stride = out_w * 4;
        let crop_x = self.crop_x as usize;
        let crop_y = self.crop_y as usize;
        // Clamp the source rectangle to the staging texture as a safety net; the
        // client area is geometrically inside the window, so in practice this is
        // exactly `out_w`/`out_h`.
        let copy_w = out_w.min((self.pool_width as usize).saturating_sub(crop_x));
        let copy_h = out_h.min((self.pool_height as usize).saturating_sub(crop_y));
        let copy_bytes = copy_w * 4;
        let data = unsafe {
            context.CopyResource(staging, &texture);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| format!("staging Map: {e}"))?;

            let mut data = vec![0u8; out_stride * out_h];
            let src_base = mapped.pData as *const u8;
            let src_pitch = mapped.RowPitch as usize;
            // Single pass over the client sub-rectangle: apply the crop offset,
            // honour the staging RowPitch, AND flip top-down → the bottom-up
            // order the scanner expects — all at once, at zero extra cost.
            for y in 0..copy_h {
                let src = src_base.add((crop_y + y) * src_pitch + crop_x * 4);
                let dst_row = (out_h - 1 - y) * out_stride;
                std::ptr::copy_nonoverlapping(src, data.as_mut_ptr().add(dst_row), copy_bytes);
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

/// Pin an implicit multithreaded apartment (MTA) for the entire process before
/// any WGC factory is touched.
///
/// windows-rs caches WinRT activation factories (here the
/// `IGraphicsCaptureSessionStatics` behind `GraphicsCaptureSession::IsSupported`)
/// in a process-wide `FactoryCache`. The cached raw pointer is only valid while
/// the activation DLL (`GraphicsCapture.dll`) stays loaded. That DLL is loaded
/// into the MTA, and the MTA lives only as long as at least one thread keeps it
/// initialized. Our `--debug` config wizard first probes WGC on a short-lived
/// worker thread: it `RoInitialize`s, populates the factory cache, then exits —
/// tearing the MTA down and unloading `GraphicsCapture.dll`. The cached pointer
/// then dangles into freed memory, and the *next* `IsSupported()` from the real
/// capture pipeline thread dereferences it → `STATUS_ACCESS_VIOLATION`.
///
/// `CoIncrementMTAUsage` creates an MTA reference that is not bound to any single
/// thread and that we intentionally never release (the cookie is dropped), so the
/// MTA — and `GraphicsCapture.dll` with it — stays alive for the whole process.
/// This makes every cached factory pointer valid regardless of which threads come
/// and go, and lets WGC factory calls succeed from threads that never call
/// `RoInitialize` themselves (they join the implicit MTA).
fn ensure_process_mta() {
    static MTA: Once = Once::new();
    MTA.call_once(|| {
        // The returned cookie is deliberately leaked: releasing it (via
        // `CoDecrementMTAUsage`) would let the MTA tear down again. Ignore errors —
        // if this somehow fails the existing per-thread `RoInitialize` still
        // applies, matching the previous behaviour.
        let _ = unsafe { CoIncrementMTAUsage() };
    });
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

/// Offset of the client area's top-left corner inside the WGC capture texture.
///
/// `CreateForWindow` captures the whole composited window, and the texture's
/// origin aligns with the window's *extended frame bounds*
/// (`DWMWA_EXTENDED_FRAME_BOUNDS`) — the visible window rect, excluding the
/// invisible drag-resize border that `GetWindowRect` reports. The client area
/// sits inside that frame, offset by the border thickness and the title bar; we
/// recover that offset by mapping both rectangles to screen coordinates and
/// subtracting. Cropping by it reproduces the GDI backend's client-only frame
/// (so calibration coordinates line up and the cost bar is not shifted down).
///
/// Both values are physical-pixel screen coordinates, matching the GDI path's
/// `ClientToScreen` usage, so the process DPI awareness is already consistent.
/// The result is clamped into the texture as a defensive bound.
fn client_crop_offset(hwnd: HWND, pool_width: u32, pool_height: u32) -> Result<(u32, u32), String> {
    let mut frame = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut frame as *mut RECT as *mut c_void,
            std::mem::size_of::<RECT>() as u32,
        )
        .map_err(|e| format!("DwmGetWindowAttribute(EXTENDED_FRAME_BOUNDS): {e}"))?;
    }
    let mut origin = POINT { x: 0, y: 0 };
    if !unsafe { ClientToScreen(hwnd, &mut origin) }.as_bool() {
        return Err("ClientToScreen failed".to_string());
    }
    let crop_x = (origin.x - frame.left).max(0) as u32;
    let crop_y = (origin.y - frame.top).max(0) as u32;
    Ok((
        crop_x.min(pool_width.saturating_sub(1)),
        crop_y.min(pool_height.saturating_sub(1)),
    ))
}
