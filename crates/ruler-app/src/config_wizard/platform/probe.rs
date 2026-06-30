//! Target discovery and the per-candidate probe workers. Each discovered
//! candidate gets a background thread that connects, captures a frame at ~4 Hz,
//! and streams latency + preview back to the UI thread over an mpsc channel;
//! the UI thread drains those messages and folds them into the [`WizardCore`].

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

use super::WizardCore;
use crate::target_discovery::{
    discover_targets, latency_class, LatencyClass, PreviewFrame, TargetCandidate,
};

const LATENCY_SAMPLE_WINDOW: usize = 12;

#[cfg(test)]
mod tests;
mod worker;

use worker::{send_probe_error, spawn_probe_worker};
pub(super) use worker::{stop_probe_worker_list, ProbeWorker};

#[derive(Clone, Debug)]
pub(super) struct ProbeMessage {
    generation: u64,
    fingerprint: String,
    result: Result<(Duration, PreviewFrame), String>,
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
