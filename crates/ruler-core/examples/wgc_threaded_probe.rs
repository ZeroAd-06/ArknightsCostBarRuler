//! Reproduce the capture pipeline's cross-thread usage of the WGC backend:
//! `connect()` runs on the start thread, then the backend is moved to a second
//! thread that runs `capture_frame()` in a loop. The bench and the wizard probe
//! both keep connect+capture on one thread, so this exercises the only path the
//! real pipeline uses that they do not.
//!
//! Usage: cargo run --release -p ruler-core --example wgc_threaded_probe -- [frames]

use std::thread;

use ruler_core::{capture::create_backend, config::RulerConfig};

fn main() {
    let frames: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(120);

    let config = RulerConfig::load_from_path("./config.json").expect("load config.json");
    let capture_config = config.to_capture_config().expect("to_capture_config");

    // --- start thread: create + connect (no COM objects created yet) ---
    eprintln!("[start] create_backend + connect()");
    let mut backend = create_backend(capture_config).expect("create_backend");
    backend.connect().expect("connect");
    let dims = backend.dimensions();
    eprintln!("[start] connected, dimensions={:?}", dims);

    // --- move backend to a fresh capture thread ---
    let handle = thread::Builder::new()
        .name("repro-capture".to_string())
        .spawn(move || {
            eprintln!("[capture] first capture_frame() (lazy_init runs here)");
            for i in 0..frames {
                match backend.capture_frame() {
                    Ok(frame) => {
                        if i == 0 || i + 1 == frames {
                            eprintln!(
                                "[capture] frame {i}: {}x{} {:?} len={}",
                                frame.width,
                                frame.height,
                                frame.format,
                                frame.data.len()
                            );
                        }
                    }
                    Err(e) => eprintln!("[capture] frame {i} error: {e}"),
                }
            }
            eprintln!("[capture] disconnect()");
            backend.disconnect();
            eprintln!("[capture] done");
        })
        .expect("spawn capture thread");

    handle.join().expect("join capture thread");
    eprintln!("[start] OK — no crash");
}
