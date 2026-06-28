use std::{
    env, fs,
    path::{Path, PathBuf},
};

use ruler_core::RulerConfig;
use serde::{Deserialize, Serialize};

const APP_DATA_DIR_NAME: &str = "ArknightsCostBarRuler";
const IDENTITY_FILE_NAME: &str = "telemetry_identity.json";
const IDENTITY_PATH_ENV: &str = "ARKNIGHTS_RULER_TELEMETRY_IDENTITY_PATH";

#[derive(Debug, Deserialize, Serialize)]
struct TelemetryIdentity {
    uuid: String,
}

pub fn ensure_config_uuid(config: &mut RulerConfig) {
    let identity_path = stable_identity_path();
    ensure_config_uuid_at(config, identity_path.as_deref());
}

pub fn config_or_stable_uuid(config: &RulerConfig) -> Option<String> {
    let identity_path = stable_identity_path();
    config_or_stable_uuid_at(config, identity_path.as_deref())
}

fn ensure_config_uuid_at(config: &mut RulerConfig, identity_path: Option<&Path>) {
    if let Some(uuid) = config_or_stable_uuid_at(config, identity_path) {
        config.uuid = Some(uuid);
        return;
    }

    let uuid = new_uuid();
    persist_identity_if_possible(identity_path, &uuid);
    config.uuid = Some(uuid);
}

fn config_or_stable_uuid_at(config: &RulerConfig, identity_path: Option<&Path>) -> Option<String> {
    if let Some(uuid) = normalized_uuid(config.uuid.as_deref()) {
        persist_identity_if_possible(identity_path, &uuid);
        return Some(uuid);
    }

    identity_path.and_then(read_identity_uuid)
}

fn new_uuid() -> String {
    uuid::Uuid::new_v4().hyphenated().to_string()
}

fn stable_identity_path() -> Option<PathBuf> {
    env_path(IDENTITY_PATH_ENV).or_else(appdata_identity_path)
}

fn appdata_identity_path() -> Option<PathBuf> {
    env_path("LOCALAPPDATA")
        .or_else(|| env_path("APPDATA"))
        .or_else(|| env_path("XDG_STATE_HOME"))
        .map(|root| root.join(APP_DATA_DIR_NAME).join(IDENTITY_FILE_NAME))
}

fn env_path(key: &str) -> Option<PathBuf> {
    let value = env::var_os(key)?;
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

fn read_identity_uuid(path: &Path) -> Option<String> {
    let contents = fs::read_to_string(path).ok()?;
    match serde_json::from_str::<TelemetryIdentity>(&contents) {
        Ok(identity) => normalized_uuid(Some(&identity.uuid)),
        Err(error) => {
            log::warn!(
                "failed to parse telemetry identity '{}': {error}",
                path.display()
            );
            None
        }
    }
}

fn persist_identity_if_possible(identity_path: Option<&Path>, uuid: &str) {
    if let Some(path) = identity_path {
        if let Err(error) = persist_identity(path, uuid) {
            log::warn!("{error}");
        }
    }
}

fn persist_identity(path: &Path, uuid: &str) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create telemetry identity dir '{}': {error}",
                parent.display()
            )
        })?;
    }

    let contents = serde_json::to_string_pretty(&TelemetryIdentity {
        uuid: uuid.to_string(),
    })
    .map_err(|error| format!("failed to serialize telemetry identity: {error}"))?;
    fs::write(path, contents).map_err(|error| {
        format!(
            "failed to write telemetry identity '{}': {error}",
            path.display()
        )
    })
}

fn normalized_uuid(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf, time::SystemTime};

    #[test]
    fn restores_uuid_from_stable_identity_when_config_has_none() {
        let root = unique_temp_dir("ruler_identity_restore");
        let identity_path = root.join("identity.json");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &identity_path,
            r#"{
  "uuid": "11111111-1111-4111-8111-111111111111"
}"#,
        )
        .unwrap();
        let mut config = minimal_config(None);

        ensure_config_uuid_at(&mut config, Some(&identity_path));

        assert_eq!(
            config.uuid.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn adopts_existing_config_uuid_into_stable_identity() {
        let root = unique_temp_dir("ruler_identity_adopt");
        let identity_path = root.join("identity.json");
        let mut config = minimal_config(Some("22222222-2222-4222-8222-222222222222".to_string()));

        ensure_config_uuid_at(&mut config, Some(&identity_path));

        let saved = fs::read_to_string(&identity_path).unwrap();
        assert!(saved.contains("22222222-2222-4222-8222-222222222222"));
        assert_eq!(
            config.uuid.as_deref(),
            Some("22222222-2222-4222-8222-222222222222")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn generates_and_persists_uuid_when_no_identity_exists() {
        let root = unique_temp_dir("ruler_identity_generate");
        let identity_path = root.join("identity.json");
        let mut config = minimal_config(None);

        ensure_config_uuid_at(&mut config, Some(&identity_path));

        let uuid = config.uuid.as_deref().unwrap();
        assert!(!uuid.trim().is_empty());
        let saved = fs::read_to_string(&identity_path).unwrap();
        assert!(saved.contains(uuid));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn telemetry_uuid_migrates_existing_config_uuid() {
        let root = unique_temp_dir("ruler_identity_telemetry_migrate");
        let identity_path = root.join("identity.json");
        let config = minimal_config(Some("33333333-3333-4333-8333-333333333333".to_string()));

        let uuid = config_or_stable_uuid_at(&config, Some(&identity_path));

        assert_eq!(
            uuid.as_deref(),
            Some("33333333-3333-4333-8333-333333333333")
        );
        let saved = fs::read_to_string(&identity_path).unwrap();
        assert!(saved.contains("33333333-3333-4333-8333-333333333333"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn telemetry_uuid_uses_stable_identity_when_config_has_none() {
        let root = unique_temp_dir("ruler_identity_telemetry_restore");
        let identity_path = root.join("identity.json");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &identity_path,
            r#"{
  "uuid": "44444444-4444-4444-8444-444444444444"
}"#,
        )
        .unwrap();
        let config = minimal_config(None);

        let uuid = config_or_stable_uuid_at(&config, Some(&identity_path));

        assert_eq!(
            uuid.as_deref(),
            Some("44444444-4444-4444-8444-444444444444")
        );
        let _ = fs::remove_dir_all(root);
    }

    fn minimal_config(uuid: Option<String>) -> RulerConfig {
        RulerConfig {
            uuid,
            telemetry_enabled: Some(true),
            screenshot_delay_ms: None,
            capture_type: "replay".to_string(),
            install_path: None,
            instance_index: None,
            device_id: None,
            window_handle: None,
            window_title: None,
            window_class: None,
            active_calibration_profile: None,
            frame_display_mode: None,
            language: None,
            auto_select_target: false,
            target_fingerprint: None,
            overlay_pos_x: None,
            overlay_pos_y: None,
            overlay_scale: None,
            ui_scaler: None,
            debug_recording_enabled: false,
            debug_recording_video: false,
            debug_recording_csv: false,
            trace_logging_enabled: false,
            log_output_dir: None,
            replay_video_path: None,
            replay_fps: None,
        }
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{nanos}"))
    }
}
