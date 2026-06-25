//! Target discovery and the per-candidate probe workers. Each discovered
//! candidate gets a background thread that connects, captures a frame at ~4 Hz,
//! and streams latency + preview back to the UI thread over an mpsc channel;
//! the UI thread drains those messages and folds them into the [`WizardCore`].

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use ruler_core::{
    capture::{create_backend, CapturedFrame},
    RulerConfig,
};

use super::WizardCore;
use crate::target_discovery::{
    discover_targets, latency_class, LatencyClass, PreviewFrame, TargetCandidate,
};

const PROBE_LOOP_PAUSE_MS: u64 = 250;
const PROBE_RECONNECT_PAUSE_MS: u64 = 1000;
const LATENCY_SAMPLE_WINDOW: usize = 12;

#[derive(Clone, Debug)]
pub(super) struct ProbeMessage {
    generation: u64,
    fingerprint: String,
    result: Result<(Duration, PreviewFrame), String>,
}

pub(super) struct ProbeWorker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

/// Re-probe `adb` on `PATH` and any emulator-bundled `adb.exe` from
/// currently running MuMu / LDPlayer processes. Updates the process-global
/// resolver cache. Cheap (one `adb version` call per candidate) and safe
/// to call repeatedly.
pub(super) fn re_resolve_adb() {
    let candidates = crate::target_discovery::discover_emulator_adb_paths();
    match ruler_core::capture::adb_resolver::resolve_adb_with(&candidates) {
        Some(exe) => log::info!(
            "adb resolved for wizard: {} (from_path={})",
            exe.path(),
            exe.from_path()
        ),
        None => log::warn!("adb could not be resolved; wizard will show adb-unavailable banner"),
    }
}

pub(super) fn refresh_candidates(core: &mut WizardCore) {
    let preferred_fingerprint = core.selected_fingerprint().or_else(|| {
        core.previous_config
            .as_ref()
            .and_then(|config| config.target_fingerprint.clone())
    });
    let previous_probe_state = core
        .candidates
        .iter()
        .map(|candidate| {
            (
                candidate.fingerprint.clone(),
                (
                    candidate.latency,
                    candidate.latency_class,
                    candidate.preview.clone(),
                    candidate.error.clone(),
                ),
            )
        })
        .collect::<HashMap<_, _>>();

    stop_probe_worker_list(&mut core.probe_workers);
    core.probe_generation = core.probe_generation.wrapping_add(1);
    core.preview_token = None;
    // Force the next sync to re-evaluate the rows. The structure signature is
    // left intact so an unchanged candidate set updates in place rather than
    // tearing down the repeater.
    core.rows_content_sig = None;

    core.candidates = discover_targets(core.previous_config.as_ref());
    for candidate in &mut core.candidates {
        if let Some((latency, latency_class, preview, error)) =
            previous_probe_state.get(&candidate.fingerprint)
        {
            candidate.latency = *latency;
            candidate.latency_class = *latency_class;
            candidate.preview = preview.clone();
            candidate.error = error.clone();
        }
    }
    let active = core
        .candidates
        .iter()
        .map(|candidate| candidate.fingerprint.clone())
        .collect::<HashSet<_>>();
    core.latency_samples
        .retain(|fingerprint, _| active.contains(fingerprint));

    if core.candidates.is_empty() {
        core.selected_index = None;
        return;
    }
    let selected =
        preferred_selection_index(&core.candidates, preferred_fingerprint.as_deref()).unwrap_or(0);
    // A refresh re-discovers candidates but must not steal the selection from
    // an active manual target (the manual row stays selected, no candidate is).
    core.selected_index = (!core.manual_mode).then_some(selected);
    start_probe_workers(core);
}

fn start_probe_workers(core: &mut WizardCore) {
    let probes = core
        .candidates
        .iter()
        .map(|candidate| (candidate.fingerprint.clone(), candidate.config.clone()))
        .collect::<Vec<_>>();
    for (fingerprint, config) in probes {
        let stop = Arc::new(AtomicBool::new(false));
        match spawn_probe_worker(
            fingerprint.clone(),
            config,
            core.probe_generation,
            core.probe_tx.clone(),
            Arc::clone(&stop),
        ) {
            Ok(handle) => core.probe_workers.push(ProbeWorker {
                stop,
                handle: Some(handle),
            }),
            Err(error) => {
                send_probe_error(&core.probe_tx, core.probe_generation, &fingerprint, error)
            }
        }
    }
}

pub(super) fn drain_probe_messages(core: &mut WizardCore) {
    while let Ok(message) = core.probe_rx.try_recv() {
        if message.generation != core.probe_generation {
            continue;
        }
        let Some(index) = core
            .candidates
            .iter()
            .position(|candidate| candidate.fingerprint == message.fingerprint)
        else {
            continue;
        };
        match message.result {
            Ok((latency, preview)) => {
                let average = record_latency_sample(core, &message.fingerprint, latency);
                if let Some(candidate) = core.candidates.get_mut(index) {
                    candidate.latency = Some(average);
                    candidate.latency_class = latency_class(average);
                    candidate.preview = Some(preview);
                    candidate.error = None;
                }
            }
            Err(error) => {
                if let Some(candidate) = core.candidates.get_mut(index) {
                    candidate.error = Some(error);
                    candidate.latency_class = LatencyClass::Unknown;
                }
            }
        }
    }
}

fn record_latency_sample(core: &mut WizardCore, fingerprint: &str, sample: Duration) -> Duration {
    let samples = core
        .latency_samples
        .entry(fingerprint.to_string())
        .or_default();
    samples.push_back(sample);
    while samples.len() > LATENCY_SAMPLE_WINDOW {
        let _ = samples.pop_front();
    }
    average_duration(samples)
}

fn average_duration(samples: &VecDeque<Duration>) -> Duration {
    if samples.is_empty() {
        return Duration::ZERO;
    }
    let sum = samples
        .iter()
        .fold(0u128, |sum, sample| sum + sample.as_nanos());
    let average = sum / samples.len() as u128;
    Duration::from_nanos(average.min(u64::MAX as u128) as u64)
}

fn preferred_selection_index(
    candidates: &[TargetCandidate],
    preferred_fingerprint: Option<&str>,
) -> Option<usize> {
    preferred_fingerprint
        .and_then(|fingerprint| {
            candidates
                .iter()
                .position(|candidate| candidate.fingerprint == fingerprint)
        })
        .or_else(|| {
            candidates
                .iter()
                .position(|candidate| candidate.error.is_none())
        })
        .or_else(|| (!candidates.is_empty()).then_some(0))
}

fn spawn_probe_worker(
    fingerprint: String,
    config: RulerConfig,
    generation: u64,
    tx: Sender<ProbeMessage>,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, String> {
    let name = format!("ruler-target-probe-{fingerprint}");
    thread::Builder::new()
        .name(name)
        .spawn(move || run_probe_worker(fingerprint, config, generation, tx, stop))
        .map_err(|error| format!("failed to start probe worker: {error}"))
}

pub(super) fn stop_probe_worker_list(workers: &mut Vec<ProbeWorker>) {
    for worker in workers.iter() {
        worker.stop.store(true, Ordering::Relaxed);
    }
    for mut worker in workers.drain(..) {
        if let Some(handle) = worker.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_probe_worker(
    fingerprint: String,
    config: RulerConfig,
    generation: u64,
    tx: Sender<ProbeMessage>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        let capture_config = match config.to_capture_config() {
            Ok(config) => config,
            Err(error) => {
                send_probe_error(&tx, generation, &fingerprint, error.to_string());
                return;
            }
        };
        let mut backend = match create_backend(capture_config) {
            Ok(backend) => backend,
            Err(error) => {
                send_probe_error(&tx, generation, &fingerprint, error);
                thread::sleep(Duration::from_millis(PROBE_RECONNECT_PAUSE_MS));
                continue;
            }
        };
        if let Err(error) = backend.connect() {
            send_probe_error(&tx, generation, &fingerprint, error);
            backend.disconnect();
            thread::sleep(Duration::from_millis(PROBE_RECONNECT_PAUSE_MS));
            continue;
        }

        while !stop.load(Ordering::Relaxed) {
            let start = std::time::Instant::now();
            match backend.capture_frame() {
                Ok(frame) => {
                    let latency = start.elapsed();
                    let preview = preview_from_captured_frame(frame);
                    if tx
                        .send(ProbeMessage {
                            generation,
                            fingerprint: fingerprint.clone(),
                            result: Ok((latency, preview)),
                        })
                        .is_err()
                    {
                        backend.disconnect();
                        return;
                    }
                }
                Err(error) => {
                    send_probe_error(&tx, generation, &fingerprint, error);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(PROBE_LOOP_PAUSE_MS));
        }
        backend.disconnect();
    }
}

fn send_probe_error(tx: &Sender<ProbeMessage>, generation: u64, fingerprint: &str, error: String) {
    let _ = tx.send(ProbeMessage {
        generation,
        fingerprint: fingerprint.to_string(),
        result: Err(error),
    });
}

fn preview_from_captured_frame(frame: CapturedFrame) -> PreviewFrame {
    PreviewFrame {
        data: frame.data,
        width: frame.width,
        height: frame.height,
        format: frame.format,
    }
}

#[cfg(test)]
mod tests {
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
}
