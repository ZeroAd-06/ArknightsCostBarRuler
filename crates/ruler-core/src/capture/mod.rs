use crate::analysis::scanner::PixelFormat;

pub mod adb;
pub mod ldplayer;
pub mod mumu;
pub mod windows;

pub use adb::AdbController;
pub use ldplayer::LDPlayerController;
pub use mumu::MuMuController;
pub use windows::WindowsController;

pub struct CapturedFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

pub trait CaptureBackend: Send {
    fn connect(&mut self) -> Result<(), String>;
    fn capture_frame(&mut self) -> Result<CapturedFrame, String>;
    fn disconnect(&mut self);
    fn dimensions(&self) -> (u32, u32);
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureType {
    Adb,
    MuMu,
    LDPlayer,
    Windows,
}

pub fn create_backend(config: CaptureConfig) -> Result<Box<dyn CaptureBackend>, String> {
    match config.capture_type {
        CaptureType::Adb => Ok(Box::new(AdbController::new(config.device_id))),
        CaptureType::MuMu => {
            let install_path = config
                .install_path
                .ok_or_else(|| "MuMu capture requires install_path".to_string())?;
            Ok(Box::new(MuMuController::new(
                install_path,
                config.instance_index,
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
    }
}
