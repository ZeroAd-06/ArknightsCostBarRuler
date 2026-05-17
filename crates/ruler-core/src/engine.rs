use std::path::Path;

use crate::analysis::calibration::LoadedCalibration;
use crate::analysis::roi::{self, Roi};
use crate::analysis::scanner::{self, PixelFormat};
use crate::capture::{create_backend, CaptureBackend, CaptureConfig, CapturedFrame};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameResult {
    pub logical_frame: Option<i32>,
    pub total_frames_in_cycle: i32,
    pub raw_pixel_width: Option<i32>,
    pub elapsed_frames: i32,
}

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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EngineStatus {
    pub connected: bool,
    pub has_calibration: bool,
    pub roi_ready: bool,
}

impl Default for RulerEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl RulerEngine {
    pub fn new() -> Self {
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

    pub fn connect(&mut self, config: CaptureConfig) -> Result<(u32, u32), String> {
        let mut backend = create_backend(config)?;
        backend.connect()?;

        let dims = backend.dimensions();
        self.roi = Some(roi::find_cost_bar_roi(dims.0 as i32, dims.1 as i32));
        self.backend = Some(backend);

        Ok(dims)
    }

    pub fn load_calibration<P: AsRef<Path>>(&mut self, path: P) -> Result<(), String> {
        let loaded = LoadedCalibration::from_file(path.as_ref())?;
        self.set_loaded_calibration(loaded);
        Ok(())
    }

    pub fn load_calibration_json(&mut self, json: &str) -> Result<(), String> {
        let loaded = LoadedCalibration::from_json(json)?;
        self.set_loaded_calibration(loaded);
        Ok(())
    }

    pub fn capture_and_analyze(&mut self) -> Result<FrameResult, String> {
        let frame_data = self.capture_frame()?;
        self.analyze_captured_frame(&frame_data)
    }

    pub fn capture_frame(&mut self) -> Result<CapturedFrame, String> {
        let mut backend = self
            .backend
            .take()
            .ok_or_else(|| "Not connected".to_string())?;

        let frame_data: CapturedFrame = backend.capture_frame()?;
        self.backend = Some(backend);

        Ok(frame_data)
    }

    pub fn analyze_captured_frame(&mut self, frame_data: &CapturedFrame) -> Result<FrameResult, String> {
        self.analyze_frame(
            &frame_data.data,
            frame_data.width,
            frame_data.height,
            frame_data.format,
        )
    }

    pub fn analyze_raw_buffer(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<FrameResult, String> {
        self.analyze_frame(buffer, width, height, format)
    }

    pub fn set_roi(&mut self, screen_width: i32, screen_height: i32) {
        self.roi = Some(roi::find_cost_bar_roi(screen_width, screen_height));
    }

    pub fn set_roi_value(&mut self, roi: Roi) {
        self.roi = Some(roi);
    }

    pub fn roi(&self) -> Option<Roi> {
        self.roi
    }

    pub fn status(&self) -> EngineStatus {
        EngineStatus {
            connected: self.backend.is_some(),
            has_calibration: self.calibration.is_some(),
            roi_ready: self.roi.is_some(),
        }
    }

    pub fn reset_timer(&mut self) {
        self.timer_offset_frames = 0;
        self.cycle_base_frames = 0;
        self.cycle_counter = 0;
        self.last_known_total_frames = 0;
        self.previous_logical_frame = -1;
    }

    pub fn adjust_timer(&mut self, frames: i32) {
        self.timer_offset_frames += frames;
        self.last_known_total_frames += frames;
    }

    pub fn set_profile_index(&mut self, index: usize) {
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

    pub fn disconnect(&mut self) {
        if let Some(mut backend) = self.backend.take() {
            backend.disconnect();
        }
    }

    fn set_loaded_calibration(&mut self, loaded: LoadedCalibration) {
        self.calibration = Some(loaded);
        self.current_profile_index = 0;
        self.cycle_counter = 0;
        self.cycle_base_frames = 0;
        self.previous_logical_frame = -1;
        self.last_known_total_frames = self.timer_offset_frames;
    }

    fn analyze_frame(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<FrameResult, String> {
        let calibration = self
            .calibration
            .as_ref()
            .ok_or_else(|| "No calibration loaded".to_string())?;
        let roi = self.roi.ok_or_else(|| {
            "No ROI set - call connect() first or use set_roi()/set_roi_value()".to_string()
        })?;

        let pixel_width = scanner::get_raw_filled_pixel_width(buffer, width, height, format, roi);

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

        Ok(FrameResult {
            logical_frame,
            total_frames_in_cycle: table.total_frames,
            raw_pixel_width: pixel_width,
            elapsed_frames: self.last_known_total_frames,
        })
    }
}

impl Drop for RulerEngine {
    fn drop(&mut self) {
        self.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_raw_buffer_tracks_frames() {
        let mut engine = RulerEngine::new();
        engine
            .load_calibration_json(
                r#"{
                    "profiles": [{
                        "total_frames": 30,
                        "pixel_map": {"0": 0, "5": 1, "10": 2}
                    }]
                }"#,
            )
            .unwrap();
        engine.set_roi_value((0, 20, 0));

        let mut buffer = vec![30u8; 20 * 3];
        buffer[0] = 252;
        buffer[1] = 252;
        buffer[2] = 252;
        buffer[3] = 252;
        buffer[4] = 252;
        buffer[5] = 252;

        let result = engine
            .analyze_raw_buffer(&buffer, 20, 1, PixelFormat::Bgr)
            .unwrap();

        assert_eq!(result.raw_pixel_width, Some(2));
        assert_eq!(result.logical_frame, Some(0));
        assert_eq!(result.elapsed_frames, 0);
    }
}
