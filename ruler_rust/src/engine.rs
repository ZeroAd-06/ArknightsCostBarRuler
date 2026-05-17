use pyo3::prelude::*;

use crate::analysis::calibration::LoadedCalibration;
use crate::analysis::roi::{self, Roi};
use crate::analysis::scanner::{self, PixelFormat};
use crate::capture::{create_backend, CaptureBackend, CaptureConfig, CapturedFrame};

#[pyclass(unsendable)]
pub struct RulerEngine {
    backend: Option<Box<dyn CaptureBackend>>,
    calibration: Option<LoadedCalibration>,
    roi: Option<Roi>,
    current_profile_index: usize,
    cycle_counter: usize,
    cycle_base_frames: i32,
    timer_offset_frames: i32,
    previous_logical_frame: i32,
    last_known_total_frames: i32,
}

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

#[pymethods]
impl RulerEngine {
    #[new]
    fn new() -> Self {
        Self {
            backend: None,
            calibration: None,
            roi: None,
            current_profile_index: 0,
            cycle_counter: 0,
            cycle_base_frames: 0,
            timer_offset_frames: 0,
            previous_logical_frame: -1,
            last_known_total_frames: 0,
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
        let config = CaptureConfig {
            capture_type: match capture_type {
                "mumu" => crate::capture::CaptureType::MuMu,
                "ldplayer" => crate::capture::CaptureType::LDPlayer,
                "window" | "windows" => crate::capture::CaptureType::Windows,
                _ => {
                    return Err(pyo3::exceptions::PyValueError::new_err(format!(
                        "Unknown capture type: {capture_type}"
                    )))
                }
            },
            install_path: install_path.map(String::from),
            instance_index: instance_index.unwrap_or(0),
            device_id: device_id.map(String::from),
            window_handle,
            window_title: window_title.map(String::from),
            window_class: window_class.map(String::from),
        };

        let mut backend = create_backend(config).map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
        backend
            .connect()
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        let dims = backend.dimensions();
        self.roi = Some(roi::find_cost_bar_roi(dims.0 as i32, dims.1 as i32));
        self.backend = Some(backend);

        Ok(dims)
    }

    fn load_calibration(&mut self, path: &str) -> PyResult<()> {
        let loaded = LoadedCalibration::from_file(std::path::Path::new(path))
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
        self.calibration = Some(loaded);
        self.current_profile_index = 0;
        self.cycle_counter = 0;
        self.cycle_base_frames = 0;
        self.previous_logical_frame = -1;
        self.last_known_total_frames = self.timer_offset_frames;
        Ok(())
    }

    fn load_calibration_json(&mut self, json: &str) -> PyResult<()> {
        let loaded = LoadedCalibration::from_json(json)
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
        self.calibration = Some(loaded);
        self.current_profile_index = 0;
        self.cycle_counter = 0;
        self.cycle_base_frames = 0;
        self.previous_logical_frame = -1;
        self.last_known_total_frames = self.timer_offset_frames;
        Ok(())
    }

    fn capture_and_analyze(&mut self, py: Python<'_>) -> PyResult<PyFrameResult> {
        let calibration = self
            .calibration
            .as_ref()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("No calibration loaded"))?;
        let roi = self
            .roi
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("No ROI set"))?;

        let mut backend_box = self
            .backend
            .take()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Not connected"))?;

        let frame_data: CapturedFrame = py
            .allow_threads(|| backend_box.capture_frame())
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        self.backend = Some(backend_box);

        let pixel_width = scanner::get_raw_filled_pixel_width(
            &frame_data.data,
            frame_data.width,
            frame_data.height,
            frame_data.format,
            roi,
        );

        let num_profiles = calibration.tables.len();
        let base_profile = if num_profiles == 0 {
            0
        } else {
            self.current_profile_index.min(num_profiles - 1)
        };
        let profile_idx = if num_profiles == 0 {
            0
        } else {
            (base_profile + self.cycle_counter) % num_profiles
        };
        let table = &calibration.tables[profile_idx];

        let logical_frame = pixel_width.and_then(|pw| table.lookup(pw));

        if let Some(lf) = logical_frame {
            let total_f = table.total_frames;
            if self.previous_logical_frame > (total_f as f64 * 0.75) as i32
                && lf < (total_f as f64 * 0.25) as i32
            {
                self.cycle_base_frames += total_f;
                self.cycle_counter += 1;
            }
            self.last_known_total_frames = self.timer_offset_frames + self.cycle_base_frames + lf;
            self.previous_logical_frame = lf;
        } else {
            self.previous_logical_frame = -1;
        }

        Ok(PyFrameResult {
            logical_frame,
            total_frames_in_cycle: table.total_frames,
            raw_pixel_width: pixel_width,
            elapsed_frames: self.last_known_total_frames,
        })
    }

    fn reset_timer(&mut self) {
        self.timer_offset_frames = 0;
        self.cycle_base_frames = 0;
        self.cycle_counter = 0;
        self.last_known_total_frames = 0;
        self.previous_logical_frame = -1;
    }

    fn adjust_timer(&mut self, frames: i32) {
        self.timer_offset_frames += frames;
        self.last_known_total_frames += frames;
    }

    fn set_profile_index(&mut self, index: usize) {
        if let Some(cal) = &self.calibration {
            if index < cal.tables.len() {
                let offset = self.cycle_base_frames;
                self.timer_offset_frames += offset;
                self.cycle_base_frames = 0;
                self.cycle_counter = 0;
                self.previous_logical_frame = -1;
                self.current_profile_index = index;
            }
        }
    }

    fn disconnect(&mut self) {
        if let Some(mut backend) = self.backend.take() {
            backend.disconnect();
        }
    }

    /// Analyze raw RGBA/BGR buffer directly. `format`: "rgba" or "bgr".
    #[pyo3(signature = (buffer, width, height, format="rgba"))]
    fn analyze_raw_buffer(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        format: &str,
    ) -> PyResult<PyFrameResult> {
        let calibration = self
            .calibration
            .as_ref()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("No calibration loaded"))?;
        let roi = self
            .roi
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("No ROI set - call connect() first or use set_roi()"))?;

        let pixel_format = match format.to_lowercase().as_str() {
            "rgba" => PixelFormat::Rgba,
            "bgr" => PixelFormat::Bgr,
            _ => return Err(pyo3::exceptions::PyValueError::new_err("format must be 'rgba' or 'bgr'")),
        };

        let pixel_width = scanner::get_raw_filled_pixel_width(
            buffer,
            width,
            height,
            pixel_format,
            roi,
        );

        let num_profiles = calibration.tables.len();
        let base_profile = if num_profiles == 0 { 0 } else { self.current_profile_index.min(num_profiles - 1) };
        let profile_idx = if num_profiles == 0 { 0 } else { (base_profile + self.cycle_counter) % num_profiles };
        let table = &calibration.tables[profile_idx];

        let logical_frame = pixel_width.and_then(|pw| table.lookup(pw));

        if let Some(lf) = logical_frame {
            let total_f = table.total_frames;
            if self.previous_logical_frame > (total_f as f64 * 0.75) as i32
                && lf < (total_f as f64 * 0.25) as i32
            {
                self.cycle_base_frames += total_f;
                self.cycle_counter += 1;
            }
            self.last_known_total_frames = self.timer_offset_frames + self.cycle_base_frames + lf;
            self.previous_logical_frame = lf;
        }

        Ok(PyFrameResult {
            logical_frame,
            total_frames_in_cycle: table.total_frames,
            raw_pixel_width: pixel_width,
            elapsed_frames: self.last_known_total_frames,
        })
    }

    #[pyo3(signature = (screen_width, screen_height))]
    fn set_roi(&mut self, screen_width: i32, screen_height: i32) {
        self.roi = Some(roi::find_cost_bar_roi(screen_width, screen_height));
    }
}

impl Drop for RulerEngine {
    fn drop(&mut self) {
        self.disconnect();
    }
}

#[allow(dead_code)]
fn _assert_imports(_: PixelFormat) {}
