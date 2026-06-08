use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use ruler_core::{
    CaptureConfig, CaptureType, FrameResult, PixelFormat, RulerEngine as CoreRulerEngine,
};

#[pyclass]
#[derive(Clone)]
pub struct PyFrameResult {
    #[pyo3(get)]
    pub logical_frame: Option<i32>,
    #[pyo3(get)]
    pub total_frames_in_cycle: i32,
    #[pyo3(get)]
    pub raw_pixel_width: Option<i32>,
    #[pyo3(get)]
    pub elapsed_frames: i32,
}

impl From<FrameResult> for PyFrameResult {
    fn from(value: FrameResult) -> Self {
        Self {
            logical_frame: value.logical_frame,
            total_frames_in_cycle: value.total_frames_in_cycle,
            raw_pixel_width: value.raw_pixel_width,
            elapsed_frames: value.elapsed_frames,
        }
    }
}

#[pyclass(unsendable)]
pub struct RulerEngine {
    inner: CoreRulerEngine,
}

#[pymethods]
impl RulerEngine {
    #[new]
    fn new() -> Self {
        Self {
            inner: CoreRulerEngine::new(),
        }
    }

    #[pyo3(signature = (capture_type, install_path=None, instance_index=None, device_id=None, window_handle=None, window_title=None, window_class=None))]
    fn connect(
        &mut self,
        capture_type: &str,
        install_path: Option<&str>,
        instance_index: Option<u32>,
        device_id: Option<&str>,
        window_handle: Option<isize>,
        window_title: Option<&str>,
        window_class: Option<&str>,
    ) -> PyResult<(u32, u32)> {
        let capture_type = match capture_type {
            "mumu" => CaptureType::MuMu,
            "ldplayer" => CaptureType::LDPlayer,
            "window" | "windows" => CaptureType::Windows,
            _ => {
                return Err(PyValueError::new_err(format!(
                    "Unknown capture type: {capture_type}"
                )))
            }
        };

        self.inner
            .connect(CaptureConfig {
                capture_type,
                install_path: install_path.map(str::to_owned),
                instance_index: instance_index.unwrap_or(0),
                device_id: device_id.map(str::to_owned),
                window_handle,
                window_title: window_title.map(str::to_owned),
                window_class: window_class.map(str::to_owned),
            })
            .map_err(PyRuntimeError::new_err)
    }

    fn load_calibration(&mut self, path: &str) -> PyResult<()> {
        self.inner
            .load_calibration(path)
            .map_err(PyRuntimeError::new_err)
    }

    fn load_calibration_json(&mut self, json: &str) -> PyResult<()> {
        self.inner
            .load_calibration_json(json)
            .map_err(PyRuntimeError::new_err)
    }

    fn capture_and_analyze(&mut self, py: Python<'_>) -> PyResult<PyFrameResult> {
        py.allow_threads(|| self.inner.capture_and_analyze())
            .map(PyFrameResult::from)
            .map_err(PyRuntimeError::new_err)
    }

    #[pyo3(signature = (buffer, width, height, format="rgba"))]
    fn analyze_raw_buffer(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        format: &str,
    ) -> PyResult<PyFrameResult> {
        let format = match format.to_ascii_lowercase().as_str() {
            "rgba" => PixelFormat::Rgba,
            "bgr" => PixelFormat::Bgr,
            _ => return Err(PyValueError::new_err("format must be 'rgba' or 'bgr'")),
        };

        self.inner
            .analyze_raw_buffer(buffer, width, height, format)
            .map(PyFrameResult::from)
            .map_err(PyRuntimeError::new_err)
    }

    #[pyo3(signature = (screen_width, screen_height))]
    fn set_roi(&mut self, screen_width: i32, screen_height: i32) {
        self.inner.set_roi(screen_width, screen_height);
    }

    fn reset_timer(&mut self) {
        self.inner.reset_timer();
    }

    fn adjust_timer(&mut self, frames: i32) {
        self.inner.adjust_timer(frames);
    }

    fn set_profile_index(&mut self, index: usize) {
        self.inner.set_profile_index(index);
    }

    fn disconnect(&mut self) {
        self.inner.disconnect();
    }
}

#[pymodule]
fn ruler_rust(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<RulerEngine>()?;
    m.add_class::<PyFrameResult>()?;
    Ok(())
}
