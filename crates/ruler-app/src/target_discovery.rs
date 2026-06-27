use std::path::PathBuf;
use std::time::Duration;

use ruler_core::{
    capture::{create_backend, CapturedFrame},
    PixelFormat, RulerConfig,
};

use crate::probe_capture::{capture_timed_probe_frame, warm_up_capture_backend};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetKind {
    MuMu,
    LDPlayer,
    Windows,
    Adb,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LatencyClass {
    PaleGreen,
    Green,
    Yellow,
    Orange,
    Red,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct PreviewFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

#[derive(Clone, Debug)]
pub struct TargetCandidate {
    pub kind: TargetKind,
    pub fingerprint: String,
    pub name: String,
    pub detail: String,
    pub config: RulerConfig,
    pub latency: Option<Duration>,
    pub latency_class: LatencyClass,
    pub preview: Option<PreviewFrame>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ProbeResult {
    pub fingerprint: String,
    pub latency: Option<Duration>,
    pub latency_class: LatencyClass,
    pub preview: Option<PreviewFrame>,
    pub error: Option<String>,
}

impl TargetCandidate {
    #[must_use]
    pub fn latency_text(&self) -> String {
        match self.latency {
            Some(latency) => format!("{:.1} ms", latency.as_secs_f64() * 1000.0),
            None => "measuring...".to_string(),
        }
    }

    #[must_use]
    pub fn list_label(&self) -> String {
        let status = self
            .error
            .as_ref()
            .map(|error| format!("error: {error}"))
            .unwrap_or_else(|| self.latency_text());
        format!(
            "{} | {} | {} | {}",
            kind_label(self.kind),
            self.name,
            status,
            self.detail
        )
    }
}

fn kind_label(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::MuMu => "MuMu",
        TargetKind::LDPlayer => "LDPlayer",
        TargetKind::Windows => "Windows",
        TargetKind::Adb => "ADB",
    }
}

#[must_use]
pub fn latency_class(latency: Duration) -> LatencyClass {
    let ms = latency.as_secs_f64() * 1000.0;
    if ms < 8.0 {
        LatencyClass::PaleGreen
    } else if ms <= 16.0 {
        LatencyClass::Green
    } else if ms <= 33.0 {
        LatencyClass::Yellow
    } else if ms <= 166.0 {
        LatencyClass::Orange
    } else {
        LatencyClass::Red
    }
}

#[must_use]
pub fn discover_targets(previous: Option<&RulerConfig>) -> Vec<TargetCandidate> {
    platform::discover_targets(previous)
}

/// Discover `adb.exe` paths bundled with running MuMu / LDPlayer emulators.
///
/// Returned paths are absolute and de-duplicated. The list is fed into
/// `ruler_core::capture::adb_resolver::resolve_adb_with` so that adb-using
/// capture backends fall back to a bundled adb when none is on `PATH`.
///
/// On non-Windows this always returns an empty vector (MuMu / LDPlayer are
/// Windows-only).
#[must_use]
pub fn discover_emulator_adb_paths() -> Vec<PathBuf> {
    platform::discover_emulator_adb_paths()
}

#[must_use]
pub fn probe_candidate_once(candidate: &TargetCandidate) -> ProbeResult {
    probe_config_once(candidate.fingerprint.clone(), &candidate.config)
}

#[must_use]
pub fn probe_config_once(fingerprint: String, config: &RulerConfig) -> ProbeResult {
    let capture_config = match config.to_capture_config() {
        Ok(config) => config,
        Err(error) => {
            return ProbeResult {
                fingerprint,
                latency: None,
                latency_class: LatencyClass::Unknown,
                preview: None,
                error: Some(error.to_string()),
            };
        }
    };
    let mut backend = match create_backend(capture_config) {
        Ok(backend) => backend,
        Err(error) => {
            return ProbeResult {
                fingerprint,
                latency: None,
                latency_class: LatencyClass::Unknown,
                preview: None,
                error: Some(error),
            };
        }
    };

    let result = (|| {
        backend.connect()?;
        warm_up_capture_backend(backend.as_mut())?;
        capture_timed_probe_frame(backend.as_mut())
    })();
    backend.disconnect();

    match result {
        Ok((latency, frame)) => ProbeResult {
            fingerprint,
            latency: Some(latency),
            latency_class: latency_class(latency),
            preview: Some(preview_from_frame(frame)),
            error: None,
        },
        Err(error) => ProbeResult {
            fingerprint,
            latency: None,
            latency_class: LatencyClass::Unknown,
            preview: None,
            error: Some(error),
        },
    }
}

fn preview_from_frame(frame: CapturedFrame) -> PreviewFrame {
    PreviewFrame {
        data: frame.data,
        width: frame.width,
        height: frame.height,
        format: frame.format,
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub fn discover_targets(_: Option<&RulerConfig>) -> Vec<TargetCandidate> {
        Vec::new()
    }

    pub fn discover_emulator_adb_paths() -> Vec<PathBuf> {
        Vec::new()
    }
}

#[cfg(windows)]
mod platform;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn latency_boundaries_match_plan() {
        assert_eq!(
            latency_class(Duration::from_micros(7999)),
            LatencyClass::PaleGreen
        );
        assert_eq!(latency_class(Duration::from_millis(8)), LatencyClass::Green);
        assert_eq!(
            latency_class(Duration::from_millis(16)),
            LatencyClass::Green
        );
        assert_eq!(
            latency_class(Duration::from_millis(17)),
            LatencyClass::Yellow
        );
        assert_eq!(
            latency_class(Duration::from_millis(33)),
            LatencyClass::Yellow
        );
        assert_eq!(
            latency_class(Duration::from_millis(34)),
            LatencyClass::Orange
        );
        assert_eq!(
            latency_class(Duration::from_millis(166)),
            LatencyClass::Orange
        );
        assert_eq!(latency_class(Duration::from_millis(167)), LatencyClass::Red);
    }
}
