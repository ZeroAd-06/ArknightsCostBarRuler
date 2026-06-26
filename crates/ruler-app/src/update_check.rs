use std::{fmt, sync::Arc, thread, time::Duration};

use reqwest::{blocking::Client, Url};
use semver::Version;
use serde::Deserialize;

use crate::{ui_state::UpdateNotice, worker::SharedAppState};

const UPDATE_URL: &str = "https://arkruler-telemetry.z060606060606.online/update";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Debug)]
enum UpdateCheckError {
    Client(reqwest::Error),
    Request(reqwest::Error),
    Body(reqwest::Error),
}

impl fmt::Display for UpdateCheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client(error) => write!(f, "failed to build update-check HTTP client: {error}"),
            Self::Request(error) => write!(f, "failed to request update metadata: {error}"),
            Self::Body(error) => write!(f, "failed to read update metadata body: {error}"),
        }
    }
}

#[derive(Deserialize)]
struct UpdateResponse {
    version: String,
    release_title: String,
    html_url: String,
    #[serde(default)]
    download_url: Option<String>,
}

pub(crate) fn spawn_update_check(state: Arc<SharedAppState>) {
    let builder = thread::Builder::new().name("ruler-update-check".to_string());
    let result = builder.spawn(move || match fetch_update_notice() {
        Ok(Some(notice)) => {
            log::info!(
                "new ruler version available: {} ({})",
                notice.version,
                notice.html_url
            );
            state.set_update_notice(Some(notice));
        }
        Ok(None) => log::info!("no ruler update available"),
        Err(error) => log::warn!("update check failed: {error}"),
    });
    if let Err(error) = result {
        log::warn!("failed to spawn update-check thread: {error}");
    }
}

fn fetch_update_notice() -> Result<Option<UpdateNotice>, UpdateCheckError> {
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(format!(
            "ArknightsCostBarRuler/{}",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .map_err(UpdateCheckError::Client)?;
    let payload = client
        .get(UPDATE_URL)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(UpdateCheckError::Request)?
        .text()
        .map_err(UpdateCheckError::Body)?;
    Ok(notice_from_json(env!("CARGO_PKG_VERSION"), &payload))
}

#[must_use]
pub(crate) fn notice_from_json(current_version: &str, payload: &str) -> Option<UpdateNotice> {
    let response: UpdateResponse = serde_json::from_str(payload).ok()?;
    let current = parse_version(current_version)?;
    let remote = parse_version(&response.version)?;
    if remote <= current {
        return None;
    }
    let html_url = valid_http_url(&response.html_url)?;
    let download_url = response
        .download_url
        .as_deref()
        .and_then(valid_http_url)
        .map(str::to_string);

    Some(UpdateNotice {
        version: remote.to_string(),
        release_title: response.release_title,
        html_url: html_url.to_string(),
        download_url,
    })
}

fn parse_version(value: &str) -> Option<Version> {
    Version::parse(value.trim().trim_start_matches('v')).ok()
}

fn valid_http_url(value: &str) -> Option<&str> {
    let url = Url::parse(value).ok()?;
    match url.scheme() {
        "http" | "https" => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UPDATE_JSON: &str = r#"{
        "version": "2.3.0",
        "release_title": "Ruler 2.3.0",
        "html_url": "https://github.com/ZeroAd-06/ArknightsCostBarRuler/releases/tag/20260627",
        "download_url": "https://github.com/ZeroAd-06/ArknightsCostBarRuler/releases/download/20260627/ArknightsCostBarRuler.7z"
    }"#;

    #[test]
    fn returns_notice_when_remote_version_is_newer() {
        let notice = notice_from_json("2.2.0", UPDATE_JSON);

        assert_eq!(
            notice,
            Some(UpdateNotice {
                version: "2.3.0".to_string(),
                release_title: "Ruler 2.3.0".to_string(),
                html_url:
                    "https://github.com/ZeroAd-06/ArknightsCostBarRuler/releases/tag/20260627"
                        .to_string(),
                download_url: Some(
                    "https://github.com/ZeroAd-06/ArknightsCostBarRuler/releases/download/20260627/ArknightsCostBarRuler.7z"
                        .to_string()
                ),
            })
        );
    }

    #[test]
    fn ignores_same_version() {
        assert_eq!(notice_from_json("2.3.0", UPDATE_JSON), None);
    }

    #[test]
    fn ignores_older_version() {
        assert_eq!(notice_from_json("2.4.0", UPDATE_JSON), None);
    }

    #[test]
    fn accepts_v_prefixed_versions() {
        let notice = notice_from_json("v2.2.0", UPDATE_JSON);

        assert!(notice.is_some());
    }

    #[test]
    fn ignores_invalid_release_url() {
        let payload = r#"{
            "version": "2.3.0",
            "release_title": "Ruler 2.3.0",
            "html_url": "not a url"
        }"#;

        assert_eq!(notice_from_json("2.2.0", payload), None);
    }
}
