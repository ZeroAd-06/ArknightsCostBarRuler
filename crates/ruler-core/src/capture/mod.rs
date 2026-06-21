use crate::analysis::scanner::PixelFormat;

pub mod adb;
pub(crate) mod android_settings;
pub mod ldplayer;
pub mod mumu;
pub mod replay;
pub mod windows;

pub use adb::AdbController;
pub use ldplayer::LDPlayerController;
pub use mumu::MuMuController;
pub use replay::ReplayCaptureBackend;
pub use windows::WindowsController;

pub struct CapturedFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub client_left: i32,
    pub client_top: i32,
    pub width: u32,
    pub height: u32,
}

pub trait CaptureBackend: Send {
    fn connect(&mut self) -> Result<(), String>;
    fn capture_frame(&mut self) -> Result<CapturedFrame, String>;
    fn disconnect(&mut self);
    fn dimensions(&self) -> (u32, u32);
    fn window_info(&self) -> Option<WindowInfo> { None }
}

#[derive(Clone, Debug)]
pub struct CaptureConfig {
    pub capture_type: CaptureType,
    pub install_path: Option<String>,
    pub instance_index: u32,
    pub device_id: Option<String>,
    pub window_handle: Option<isize>,
    pub window_title: Option<String>,
    pub window_class: Option<String>,
    pub replay_hevc_path: Option<String>,
    pub replay_fps: Option<f64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureType {
    Adb,
    MuMu,
    LDPlayer,
    Windows,
    Replay,
}

pub fn create_backend(config: CaptureConfig) -> Result<Box<dyn CaptureBackend>, String> {
    log::info!(
        "creating capture backend: type={:?}, instance_index={}, device_id={:?}, window_handle={:?}",
        config.capture_type,
        config.instance_index,
        config.device_id,
        config.window_handle
    );
    match config.capture_type {
        CaptureType::Adb => Ok(Box::new(AdbController::new(config.device_id))),
        CaptureType::MuMu => {
            let install_path = config
                .install_path
                .ok_or_else(|| "MuMu capture requires install_path".to_string())?;
            Ok(Box::new(MuMuController::new(
                install_path,
                config.instance_index,
                config.device_id,
            )))
        }
        CaptureType::LDPlayer => {
            let install_path = config
                .install_path
                .ok_or_else(|| "LDPlayer capture requires install_path".to_string())?;
            Ok(Box::new(LDPlayerController::new(
                install_path,
                config.instance_index,
                config.device_id,
            )))
        }
        CaptureType::Windows => Ok(Box::new(WindowsController::new(
            config.window_handle,
            config.window_title,
            config.window_class,
        ))),
        CaptureType::Replay => {
            let hevc_path = config.replay_hevc_path.ok_or_else(|| {
                "Replay capture requires replay_hevc_path (recorded video file path)".to_string()
            })?;
            let fps = config.replay_fps.unwrap_or(60.0);
            Ok(Box::new(ReplayCaptureBackend::new(
                &hevc_path,
                fps,
                0,
                0,
                PixelFormat::Rgba,
            )))
        }
    }
}
