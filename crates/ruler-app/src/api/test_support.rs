use std::{
    net::TcpListener,
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;
use tungstenite::{connect, Message};

use crate::ui_state::{ApiFrameLookup, ApiFrameRecord};

pub(super) fn assert_found_frame(
    lookup: &ApiFrameLookup,
    requested_frame_id: u64,
    actual_frame_id: u64,
    fell_back: bool,
    fallback_reason: Option<&str>,
) {
    match lookup {
        ApiFrameLookup::Found {
            requested_frame_id: found_requested,
            record,
            fell_back: found_fell_back,
            fallback_reason: found_reason,
        } => {
            assert_eq!(*found_requested, requested_frame_id);
            assert_eq!(record.frame_id, actual_frame_id);
            assert_eq!(*found_fell_back, fell_back);
            assert_eq!(found_reason.as_deref(), fallback_reason);
        }
        ApiFrameLookup::NotRetained { .. } => panic!("expected retained frame"),
    }
}

pub(super) fn frame_record(frame_id: u64, dropped_since_previous: u64) -> ApiFrameRecord {
    ApiFrameRecord {
        frame_id,
        sample_index: frame_id,
        dropped_since_previous,
        is_running: true,
        current_frame: Some(frame_id as i32),
        total_frames_in_cycle: 30,
        total_elapsed_frames: frame_id as i32,
        active_profile: Some("normal".to_string()),
        raw_pixel_width: Some(frame_id as i32),
        cost_is_negative: false,
        battle_state: "battle".to_string(),
        capture_width: 1280,
        capture_height: 720,
        capture_format: "rgba".to_string(),
        capture_timestamp_ns: frame_id * 1_000,
        capture_duration_us: 1_500,
        timing_debug: None,
    }
}

pub(super) fn unused_bind_address() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    address.to_string()
}

pub(super) fn connect_with_retry(
    bind_address: &str,
) -> (
    tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    tungstenite::handshake::client::Response,
) {
    let url = format!("ws://{bind_address}/");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match connect(url.as_str()) {
            Ok(connected) => return connected,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("failed to connect test websocket runtime: {error}"),
        }
    }
}

pub(super) fn read_json_message(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
) -> Value {
    let message = socket.read().unwrap();
    let Message::Text(text) = message else {
        panic!("expected text websocket message");
    };
    serde_json::from_str(text.as_str()).unwrap()
}
