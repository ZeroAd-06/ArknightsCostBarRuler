use std::{sync::atomic::Ordering, thread};

use super::*;

#[test]
fn preferred_selection_keeps_existing_fingerprint_after_refresh() {
    let candidates = vec![
        candidate("adb:first", None),
        candidate("mumu:selected", None),
        candidate("ld:last", None),
    ];
    assert_eq!(
        preferred_selection_index(&candidates, Some("mumu:selected")),
        Some(1)
    );
}

#[test]
fn preferred_selection_falls_back_to_first_usable_candidate() {
    let candidates = vec![
        candidate("bad", Some("offline")),
        candidate("good", None),
        candidate("later", None),
    ];
    assert_eq!(
        preferred_selection_index(&candidates, Some("missing")),
        Some(1)
    );
}

#[test]
fn average_duration_uses_all_window_samples() {
    let samples = VecDeque::from([
        Duration::from_millis(10),
        Duration::from_millis(20),
        Duration::from_millis(30),
    ]);
    assert_eq!(average_duration(&samples), Duration::from_millis(20));
}

#[test]
fn stop_probe_worker_list_sets_stop_flag_and_joins() {
    let stop = Arc::new(AtomicBool::new(false));
    let observed_stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let worker_observed_stop = Arc::clone(&observed_stop);
    let handle = thread::spawn(move || {
        while !worker_stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(1));
        }
        worker_observed_stop.store(true, Ordering::Relaxed);
    });
    let mut workers = vec![ProbeWorker {
        stop,
        handle: Some(handle),
    }];
    stop_probe_worker_list(&mut workers);
    assert!(workers.is_empty());
    assert!(observed_stop.load(Ordering::Relaxed));
}

fn candidate(fingerprint: &str, error: Option<&str>) -> TargetCandidate {
    TargetCandidate {
        kind: crate::target_discovery::TargetKind::Adb,
        fingerprint: fingerprint.to_string(),
        name: fingerprint.to_string(),
        detail: String::new(),
        config: RulerConfig {
            uuid: None,
            telemetry_enabled: None,
            screenshot_delay_ms: None,
            capture_type: "adb".to_string(),
            install_path: None,
            instance_index: None,
            device_id: Some(fingerprint.to_string()),
            window_handle: None,
            window_title: None,
            window_class: None,
            active_calibration_profile: None,
            frame_display_mode: Some("0_to_n-1".to_string()),
            language: Some("zh_CN".to_string()),
            auto_select_target: false,
            target_fingerprint: Some(fingerprint.to_string()),
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
        },
        latency: None,
        latency_class: LatencyClass::Unknown,
        preview: None,
        error: error.map(str::to_string),
    }
}
