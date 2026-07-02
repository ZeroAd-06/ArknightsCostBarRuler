use std::{
    sync::{mpsc, Arc},
    time::Duration,
};

use serde_json::{json, Value};
use tungstenite::Message;

use super::{
    response,
    test_support::{
        assert_found_frame, connect_with_retry, frame_record, read_json_message,
        unused_bind_address,
    },
    ApiRuntime,
};
use crate::{
    commands::UiCommand,
    ui_state::{ApiFrameLookup, FrameDisplayMode},
    worker::SharedAppState,
};

#[test]
fn snapshot_json_keeps_legacy_fields_when_enhanced_fields_exist() {
    let state = SharedAppState::default();
    state.update_ui(|ui, api| {
        ui.display_mode = FrameDisplayMode::ZeroToNMinusOne;
        ui.display_frame = "15".to_string();
        ui.display_total = "/29".to_string();
        ui.time_str = "00:02:15".to_string();
        ui.lap_frames = Some(45);
        ui.can_undo_reset = true;
        api.is_running = true;
        api.current_frame = Some(15);
        api.total_frames_in_cycle = 30;
        api.total_elapsed_frames = 75;
        api.active_profile = Some("normal".to_string());
        api.frame_id = Some(42);
        api.sample_index = 7;
        api.raw_pixel_width = Some(123);
        api.cost_is_negative = true;
        api.battle_state = Some("battle".to_string());
        api.capture_width = Some(1280);
        api.capture_height = Some(720);
        api.capture_format = Some("rgba".to_string());
        api.capture_timestamp_ns = Some(9_000);
        api.capture_duration_us = Some(1_500);
    });
    state.record_api_frame(frame_record(42, 0));

    let value: Value = serde_json::from_str(&response::snapshot_json(&state)).unwrap();

    assert_eq!(value["isRunning"], true);
    assert_eq!(value["currentFrame"], 15);
    assert_eq!(value["totalFramesInCycle"], 30);
    assert_eq!(value["totalElapsedFrames"], 75);
    assert_eq!(value["activeProfile"], "normal");
    assert_eq!(value["apiVersion"], 2);
    assert_eq!(value["frameId"], 42);
    assert_eq!(value["sampleIndex"], 7);
    assert_eq!(value["droppedSincePrevious"], 0);
    assert_eq!(value["rawPixelWidth"], 123);
    assert_eq!(value["costIsNegative"], true);
    assert_eq!(value["battleState"], "battle");
    assert_eq!(value["captureWidth"], 1280);
    assert_eq!(value["captureHeight"], 720);
    assert_eq!(value["captureFormat"], "rgba");
    assert_eq!(value["captureTimestampNs"], 9_000);
    assert_eq!(value["captureDurationUs"], 1_500);
    assert_eq!(value["displayMode"], "0_to_n-1");
    assert_eq!(value["displayFrame"], "15");
    assert_eq!(value["displayTotal"], "/29");
    assert_eq!(value["time"], "00:02:15");
    assert_eq!(value["lapFrames"], 45);
    assert_eq!(value["canUndoReset"], true);
    assert_eq!(value["historyOldestFrameId"], 42);
    assert_eq!(value["historyLatestFrameId"], 42);
}

#[test]
fn api_frame_history_handles_exact_fallback_bounds_and_clear() {
    let state = SharedAppState::default();
    state.record_api_frame(frame_record(10, 0));
    state.record_api_frame(frame_record(13, 2));

    assert_found_frame(&state.api_frame_at_or_before(13), 13, 13, false, None);
    assert_found_frame(
        &state.api_frame_at_or_before(12),
        12,
        10,
        true,
        Some("frame_skipped"),
    );
    assert_found_frame(
        &state.api_frame_at_or_before(99),
        99,
        13,
        true,
        Some("requested_after_latest"),
    );

    match state.api_frame_at_or_before(9) {
        ApiFrameLookup::NotRetained { requested_frame_id } => assert_eq!(requested_frame_id, 9),
        ApiFrameLookup::Found { .. } => panic!("expected not-retained lookup"),
    }

    state.clear_api_frame_history();
    assert!(matches!(
        state.api_frame_at_or_before(13),
        ApiFrameLookup::NotRetained {
            requested_frame_id: 13
        }
    ));
}

#[test]
fn websocket_command_request_sends_ui_command_and_echoes_request_id() {
    let state = SharedAppState::default();
    let (tx, rx) = mpsc::channel();
    let response = response::handle_client_text(
        &state,
        &tx,
        r#"{"type":"adjustTimer","requestId":"r1","frames":-30}"#,
    );

    assert_eq!(
        rx.recv_timeout(Duration::from_millis(50)).unwrap(),
        UiCommand::AdjustTimer { frames: -30 }
    );
    assert_eq!(response["type"], "ack");
    assert_eq!(response["requestId"], "r1");
}

#[test]
fn websocket_set_timer_request_accepts_timer_text() {
    let state = SharedAppState::default();
    let (tx, rx) = mpsc::channel();
    let response = response::handle_client_text(
        &state,
        &tx,
        r#"{"type":"setTimer","requestId":"timer","time":"01:02:03"}"#,
    );

    assert_eq!(
        rx.recv_timeout(Duration::from_millis(50)).unwrap(),
        UiCommand::SetTimer { frames: 1_863 }
    );
    assert_eq!(response["type"], "ack");
    assert_eq!(response["requestId"], "timer");
}

#[test]
fn websocket_get_frame_returns_typed_error_when_history_misses() {
    let state = SharedAppState::default();
    let (tx, _rx) = mpsc::channel();
    let response = response::handle_client_text(
        &state,
        &tx,
        r#"{"type":"getFrame","requestId":7,"frameId":99}"#,
    );

    assert_eq!(
        response,
        json!({
            "type": "error",
            "requestId": 7,
            "code": "frame_not_retained",
            "message": "no analyzed frame retained at or before frameId 99"
        })
    );
}

#[test]
fn websocket_runtime_handles_snapshot_frame_and_command_requests() {
    let state = Arc::new(SharedAppState::default());
    state.update_ui(|_, api| {
        api.is_running = true;
        api.current_frame = Some(12);
        api.total_frames_in_cycle = 30;
        api.total_elapsed_frames = 72;
        api.active_profile = Some("normal".to_string());
        api.frame_id = Some(42);
    });
    state.record_api_frame(frame_record(42, 0));
    let (tx, rx) = mpsc::channel();
    let bind_address = unused_bind_address();
    let _runtime = ApiRuntime::new_for_bind_address(bind_address.clone(), Arc::clone(&state), tx);
    let (mut socket, _) = connect_with_retry(&bind_address);

    let initial = read_json_message(&mut socket);
    assert_eq!(initial["apiVersion"], 2);
    assert_eq!(initial["isRunning"], true);

    socket
        .send(Message::Text(
            json!({"type":"getSnapshot","requestId":"snapshot"})
                .to_string()
                .into(),
        ))
        .unwrap();
    let snapshot = read_json_message(&mut socket);
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["requestId"], "snapshot");
    assert_eq!(snapshot["payload"]["frameId"], 42);

    socket
        .send(Message::Text(
            json!({"type":"getFrame","requestId":"frame","frameId":43})
                .to_string()
                .into(),
        ))
        .unwrap();
    let frame = read_json_message(&mut socket);
    assert_eq!(frame["type"], "frame");
    assert_eq!(frame["requestId"], "frame");
    assert_eq!(frame["requestedFrameId"], 43);
    assert_eq!(frame["actualFrameId"], 42);
    assert_eq!(frame["fellBack"], true);
    assert_eq!(frame["fallbackReason"], "requested_after_latest");

    socket
        .send(Message::Text(
            json!({"type":"adjustTimer","requestId":"adjust","frames":-30})
                .to_string()
                .into(),
        ))
        .unwrap();
    let ack = read_json_message(&mut socket);
    assert_eq!(ack["type"], "ack");
    assert_eq!(ack["requestId"], "adjust");
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        UiCommand::AdjustTimer { frames: -30 }
    );

    socket
        .send(Message::Text(
            json!({"type":"setTimer","requestId":"set","frames":75})
                .to_string()
                .into(),
        ))
        .unwrap();
    let ack = read_json_message(&mut socket);
    assert_eq!(ack["type"], "ack");
    assert_eq!(ack["requestId"], "set");
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        UiCommand::SetTimer { frames: 75 }
    );
}
