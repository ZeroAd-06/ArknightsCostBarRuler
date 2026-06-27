use std::time::{Duration, Instant};

use ruler_core::capture::{CaptureBackend, CapturedFrame};

pub(crate) fn warm_up_capture_backend(backend: &mut dyn CaptureBackend) -> Result<(), String> {
    backend.capture_frame().map(|_| ())
}

pub(crate) fn capture_timed_probe_frame(
    backend: &mut dyn CaptureBackend,
) -> Result<(Duration, CapturedFrame), String> {
    let start = Instant::now();
    let frame = backend.capture_frame()?;
    Ok((start.elapsed(), frame))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use ruler_core::{
        capture::{CaptureBackend, CapturedFrame},
        PixelFormat,
    };

    use super::*;

    struct FakeCaptureBackend {
        frames: VecDeque<Result<CapturedFrame, String>>,
    }

    impl FakeCaptureBackend {
        fn new(frames: Vec<Result<CapturedFrame, String>>) -> Self {
            Self {
                frames: VecDeque::from(frames),
            }
        }
    }

    impl CaptureBackend for FakeCaptureBackend {
        fn connect(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn capture_frame(&mut self) -> Result<CapturedFrame, String> {
            self.frames
                .pop_front()
                .unwrap_or_else(|| Err("no frame prepared".to_string()))
        }

        fn disconnect(&mut self) {}

        fn dimensions(&self) -> (u32, u32) {
            (1, 1)
        }
    }

    #[test]
    fn warm_up_capture_backend_discards_first_frame() {
        let mut backend = FakeCaptureBackend::new(vec![Ok(test_frame(11)), Ok(test_frame(22))]);

        warm_up_capture_backend(&mut backend).expect("warm-up should succeed");
        let (_, frame) =
            capture_timed_probe_frame(&mut backend).expect("timed probe frame should succeed");

        assert_eq!(frame.data, vec![22]);
    }

    #[test]
    fn warm_up_capture_backend_propagates_capture_error() {
        let mut backend = FakeCaptureBackend::new(vec![Err("warm-up failed".to_string())]);

        let error = warm_up_capture_backend(&mut backend).expect_err("warm-up should fail");

        assert_eq!(error, "warm-up failed");
    }

    fn test_frame(value: u8) -> CapturedFrame {
        CapturedFrame {
            data: vec![value],
            width: 1,
            height: 1,
            format: PixelFormat::Rgba,
        }
    }
}
