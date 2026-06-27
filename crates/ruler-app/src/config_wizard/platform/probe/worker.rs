use std::{
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

use crate::probe_capture::{capture_timed_probe_frame, warm_up_capture_backend};
use crate::target_discovery::PreviewFrame;

use super::ProbeMessage;

const PROBE_LOOP_PAUSE_MS: u64 = 250;
const PROBE_RECONNECT_PAUSE_MS: u64 = 1000;

pub(crate) struct ProbeWorker {
    pub(super) stop: Arc<AtomicBool>,
    pub(super) handle: Option<JoinHandle<()>>,
}

pub(super) fn spawn_probe_worker(
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

pub(crate) fn stop_probe_worker_list(workers: &mut Vec<ProbeWorker>) {
    for worker in workers.iter() {
        worker.stop.store(true, Ordering::Relaxed);
    }
    for mut worker in workers.drain(..) {
        if let Some(handle) = worker.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(super) fn send_probe_error(
    tx: &Sender<ProbeMessage>,
    generation: u64,
    fingerprint: &str,
    error: String,
) {
    let _ = tx.send(ProbeMessage {
        generation,
        fingerprint: fingerprint.to_string(),
        result: Err(error),
    });
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
        if let Err(error) = warm_up_capture_backend(backend.as_mut()) {
            send_probe_error(&tx, generation, &fingerprint, error);
            backend.disconnect();
            thread::sleep(Duration::from_millis(PROBE_RECONNECT_PAUSE_MS));
            continue;
        }

        while !stop.load(Ordering::Relaxed) {
            match capture_timed_probe_frame(backend.as_mut()) {
                Ok((latency, frame)) => {
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

fn preview_from_captured_frame(frame: CapturedFrame) -> PreviewFrame {
    PreviewFrame {
        data: frame.data,
        width: frame.width,
        height: frame.height,
        format: frame.format,
    }
}
