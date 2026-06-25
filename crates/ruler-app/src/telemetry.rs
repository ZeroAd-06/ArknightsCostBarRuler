use std::{
    fs,
    io::{Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

use ruler_core::{pipeline::PipelineInfo, RulerConfig};
use serde::{Deserialize, Serialize};

const TELEMETRY_HOST: &str = "arkruler-telemetry.z060606060606.online";
const TELEMETRY_PATH: &str = "/";
const TELEMETRY_STATS_FILE: &str = "telemetry_stats.json";
const IO_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Default)]
pub struct RunTelemetryStats {
    analyzed_frames: AtomicU64,
    action_restarts: AtomicU64,
}

impl RunTelemetryStats {
    pub fn record_analyzed_frame(&self) {
        let _ = self.analyzed_frames.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_action_restart(&self) {
        let _ = self.action_restarts.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> SessionTelemetryStats {
        SessionTelemetryStats {
            total_frames: self.analyzed_frames.load(Ordering::Relaxed),
            restarts: self.action_restarts.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionTelemetryStats {
    pub total_frames: u64,
    pub restarts: u64,
}

#[derive(Debug, Serialize)]
struct StartupTelemetryPayload {
    uuid: String,
    client_type: String,
    version: String,
    resolution: String,
    screenshot_delay: f64,
    total_frames: u64,
    restarts: u64,
}

pub fn new_uuid() -> String {
    uuid::Uuid::new_v4().hyphenated().to_string()
}

pub fn ensure_config_uuid(config: &mut RulerConfig) {
    let has_uuid = config
        .uuid
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if !has_uuid {
        config.uuid = Some(new_uuid());
    }
}

pub fn write_session_stats(session_dir: &Path, stats: &RunTelemetryStats) -> Result<(), String> {
    fs::create_dir_all(session_dir).map_err(|error| {
        format!(
            "failed to create telemetry stats dir '{}': {error}",
            session_dir.display()
        )
    })?;
    let path = session_dir.join(TELEMETRY_STATS_FILE);
    let contents = serde_json::to_string_pretty(&stats.snapshot())
        .map_err(|error| format!("failed to serialize telemetry stats: {error}"))?;
    fs::write(&path, contents).map_err(|error| {
        format!(
            "failed to write telemetry stats '{}': {error}",
            path.display()
        )
    })
}

pub fn send_startup_telemetry(
    config: &RulerConfig,
    info: &PipelineInfo,
    current_session_dir: &Path,
) {
    if config.telemetry_enabled != Some(true) {
        return;
    }

    let Some(uuid) = config
        .uuid
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        log::warn!("telemetry enabled but config uuid is missing; skipping startup telemetry");
        return;
    };

    let previous_stats = current_session_dir
        .parent()
        .and_then(|root| read_latest_previous_stats(root, current_session_dir))
        .unwrap_or_default();

    let payload = StartupTelemetryPayload {
        uuid,
        client_type: config.capture_type.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        resolution: format!("{}x{}", info.width, info.height),
        screenshot_delay: config.screenshot_delay_ms.unwrap_or(0.0),
        total_frames: previous_stats.total_frames,
        restarts: previous_stats.restarts,
    };

    thread::Builder::new()
        .name("ruler-telemetry".to_string())
        .spawn(move || match post_payload(&payload) {
            Ok(()) => log::info!(
                "startup telemetry sent: client_type={}, resolution={}, total_frames={}, restarts={}",
                payload.client_type,
                payload.resolution,
                payload.total_frames,
                payload.restarts
            ),
            Err(error) => log::warn!("startup telemetry send failed: {error}"),
        })
        .map(|_| ())
        .unwrap_or_else(|error| log::warn!("failed to spawn telemetry thread: {error}"));
}

fn read_latest_previous_stats(
    log_root: &Path,
    current_session_dir: &Path,
) -> Option<SessionTelemetryStats> {
    let stats_path = latest_previous_stats_path(log_root, current_session_dir)?;
    let contents = fs::read_to_string(&stats_path).ok()?;
    match serde_json::from_str(&contents) {
        Ok(stats) => Some(stats),
        Err(error) => {
            log::warn!(
                "failed to parse telemetry stats '{}': {error}",
                stats_path.display()
            );
            None
        }
    }
}

fn latest_previous_stats_path(log_root: &Path, current_session_dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(log_root).ok()?;
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path == current_session_dir {
                return None;
            }
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() {
                return None;
            }
            let stats_path = path.join(TELEMETRY_STATS_FILE);
            if !stats_path.is_file() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            Some((name, stats_path))
        })
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, path)| path)
}

fn post_payload(payload: &StartupTelemetryPayload) -> Result<(), String> {
    let body = serde_json::to_vec(payload)
        .map_err(|error| format!("failed to serialize telemetry payload: {error}"))?;
    let request = build_http_request(&body);

    let mut stream = TcpStream::connect((TELEMETRY_HOST, 80))
        .map_err(|error| format!("failed to connect telemetry endpoint: {error}"))?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|error| format!("failed to set telemetry read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|error| format!("failed to set telemetry write timeout: {error}"))?;
    stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.write_all(&body))
        .map_err(|error| format!("failed to write telemetry request: {error}"))?;

    let mut response = [0u8; 128];
    let read = stream
        .read(&mut response)
        .map_err(|error| format!("failed to read telemetry response: {error}"))?;
    let status = String::from_utf8_lossy(&response[..read]);
    if status.starts_with("HTTP/1.1 2") || status.starts_with("HTTP/1.0 2") {
        Ok(())
    } else {
        Err(format!(
            "telemetry endpoint returned {}",
            status.lines().next().unwrap_or("<empty response>")
        ))
    }
}

fn build_http_request(body: &[u8]) -> String {
    format!(
        "POST {TELEMETRY_PATH} HTTP/1.1\r\n\
         Host: {TELEMETRY_HOST}\r\n\
         User-Agent: arknights-cost-bar-ruler/{}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        env!("CARGO_PKG_VERSION"),
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, time::SystemTime};

    #[test]
    fn latest_previous_stats_skips_current_session() {
        let root = unique_temp_dir("ruler_telemetry_latest");
        let previous = root.join("20260624_120000_1");
        let current = root.join("20260625_120000_2");
        fs::create_dir_all(&previous).unwrap();
        fs::create_dir_all(&current).unwrap();
        fs::write(
            previous.join(TELEMETRY_STATS_FILE),
            r#"{"total_frames":1200,"restarts":5}"#,
        )
        .unwrap();
        fs::write(
            current.join(TELEMETRY_STATS_FILE),
            r#"{"total_frames":9999,"restarts":99}"#,
        )
        .unwrap();

        let stats = read_latest_previous_stats(&root, &current).unwrap();

        assert_eq!(
            stats,
            SessionTelemetryStats {
                total_frames: 1200,
                restarts: 5
            }
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn http_request_contains_required_json_headers() {
        let request = build_http_request(br#"{"uuid":"test"}"#);

        assert!(request.starts_with("POST / HTTP/1.1\r\n"));
        assert!(request.contains("Host: arkruler-telemetry.z060606060606.online\r\n"));
        assert!(request.contains("Content-Type: application/json\r\n"));
        assert!(request.contains("Content-Length: 15\r\n"));
        assert!(request.ends_with("\r\n\r\n"));
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{nanos}"))
    }
}
