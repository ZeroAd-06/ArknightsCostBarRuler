use std::time::{Duration, Instant};

const DEFAULT_CAPTURE_INTERVAL_MS: f64 = 1000.0 / 60.0;

pub(super) fn capture_interval_from_delay_ms(delay_ms: Option<f64>) -> Duration {
    let millis = delay_ms
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(DEFAULT_CAPTURE_INTERVAL_MS);
    Duration::from_secs_f64(millis / 1000.0)
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CaptureThrottle {
    interval: Option<Duration>,
}

impl CaptureThrottle {
    pub(super) fn new(interval: Option<Duration>) -> Self {
        Self { interval }
    }

    pub(super) fn remaining_delay(&self, started: Instant, now: Instant) -> Option<Duration> {
        let interval = self.interval?;
        let elapsed = now.saturating_duration_since(started);
        if elapsed < interval {
            Some(interval - elapsed)
        } else {
            None
        }
    }

    pub(super) fn sleep_after_capture(&self, started: Instant) {
        if let Some(delay) = self.remaining_delay(started, Instant::now()) {
            std::thread::sleep(delay);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::CaptureThrottle;

    #[test]
    fn fast_capture_waits_for_remaining_interval() {
        let throttle = CaptureThrottle::new(Some(Duration::from_millis(16)));
        let started = Instant::now();

        let remaining = throttle.remaining_delay(started, started + Duration::from_millis(5));

        assert_eq!(remaining, Some(Duration::from_millis(11)));
    }

    #[test]
    fn slow_capture_does_not_wait() {
        let throttle = CaptureThrottle::new(Some(Duration::from_millis(16)));
        let started = Instant::now();

        let remaining = throttle.remaining_delay(started, started + Duration::from_millis(20));

        assert_eq!(remaining, None);
    }

    #[test]
    fn disabled_throttle_never_waits() {
        let throttle = CaptureThrottle::new(None);
        let started = Instant::now();

        let remaining = throttle.remaining_delay(started, started);

        assert_eq!(remaining, None);
    }

    #[test]
    fn capture_delay_uses_configured_positive_millis() {
        let interval = super::capture_interval_from_delay_ms(Some(18.5));

        assert_eq!(interval, Duration::from_secs_f64(0.0185));
    }

    #[test]
    fn capture_delay_falls_back_to_sixty_fps_for_missing_or_invalid_values() {
        let expected = Duration::from_secs_f64((1000.0 / 60.0) / 1000.0);

        assert_eq!(super::capture_interval_from_delay_ms(None), expected);
        assert_eq!(super::capture_interval_from_delay_ms(Some(0.0)), expected);
        assert_eq!(super::capture_interval_from_delay_ms(Some(f64::NAN)), expected);
    }
}
