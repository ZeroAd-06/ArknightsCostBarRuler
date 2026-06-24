use std::sync::mpsc::Sender;

use serde::Serialize;
use serde_json::{Map, Value};

use super::{
    payload::{ApiFramePayload, ApiPayload},
    protocol::{parse_client_request, ClientAction, ClientRequest, RequestError},
};
use crate::{commands::UiCommand, ui_state::ApiFrameLookup, worker::SharedAppState};

pub(super) fn snapshot_json(state: &SharedAppState) -> String {
    let snapshot = state.snapshot();
    let payload = ApiPayload::from_snapshot(&snapshot, state.api_history_bounds());
    serde_json::to_string(&payload).unwrap_or_else(|error| {
        log::error!("failed to encode API payload: {error}");
        "{\"apiVersion\":2,\"appVersion\":\"unknown\",\"isRunning\":false,\"currentFrame\":null,\"totalFramesInCycle\":0,\"totalElapsedFrames\":0,\"activeProfile\":null}".to_string()
    })
}

pub(super) fn handle_client_text(
    state: &SharedAppState,
    command_tx: &Sender<UiCommand>,
    text: &str,
) -> Value {
    match parse_client_request(text) {
        Ok(request) => handle_client_request(state, command_tx, request),
        Err(error) => request_error_response(error),
    }
}

fn handle_client_request(
    state: &SharedAppState,
    command_tx: &Sender<UiCommand>,
    request: ClientRequest,
) -> Value {
    match request.action {
        ClientAction::GetSnapshot => snapshot_response(request.request_id, state),
        ClientAction::GetFrame { frame_id } => frame_response(request.request_id, state, frame_id),
        ClientAction::SendCommand(command) => {
            command_response(request.request_id, command_tx, command, "command")
        }
        ClientAction::CancelCalibration => {
            state.request_cancel_calibration();
            ack_response(request.request_id, "cancelCalibration")
        }
        ClientAction::Exit => {
            state.request_cancel_calibration();
            command_response(request.request_id, command_tx, UiCommand::Exit, "exit")
        }
    }
}

fn snapshot_response(request_id: Option<Value>, state: &SharedAppState) -> Value {
    let snapshot = state.snapshot();
    let payload = ApiPayload::from_snapshot(&snapshot, state.api_history_bounds());
    let mut object = response_object("snapshot", request_id);
    object.insert("payload".to_string(), serialize_value(payload));
    Value::Object(object)
}

fn frame_response(
    request_id: Option<Value>,
    state: &SharedAppState,
    requested_frame_id: u64,
) -> Value {
    match state.api_frame_at_or_before(requested_frame_id) {
        ApiFrameLookup::Found {
            requested_frame_id,
            record,
            fell_back,
            fallback_reason,
        } => {
            let mut object = response_object("frame", request_id);
            object.insert(
                "requestedFrameId".to_string(),
                Value::from(requested_frame_id),
            );
            object.insert("actualFrameId".to_string(), Value::from(record.frame_id));
            object.insert("fellBack".to_string(), Value::from(fell_back));
            object.insert(
                "fallbackReason".to_string(),
                fallback_reason.map_or(Value::Null, Value::from),
            );
            object.insert(
                "frame".to_string(),
                serialize_value(ApiFramePayload::from_record(&record)),
            );
            Value::Object(object)
        }
        ApiFrameLookup::NotRetained { requested_frame_id } => error_response(
            request_id,
            "frame_not_retained",
            format!("no analyzed frame retained at or before frameId {requested_frame_id}"),
        ),
    }
}

fn command_response(
    request_id: Option<Value>,
    command_tx: &Sender<UiCommand>,
    command: UiCommand,
    action: &'static str,
) -> Value {
    match command_tx.send(command) {
        Ok(()) => ack_response(request_id, action),
        Err(error) => error_response(
            request_id,
            "command_channel_closed",
            format!("failed to send command to worker: {error}"),
        ),
    }
}

fn request_error_response(error: RequestError) -> Value {
    error_response(error.request_id, error.code, error.message)
}

fn ack_response(request_id: Option<Value>, action: &'static str) -> Value {
    let mut object = response_object("ack", request_id);
    object.insert("action".to_string(), Value::from(action));
    Value::Object(object)
}

pub(super) fn error_response(
    request_id: Option<Value>,
    code: &'static str,
    message: String,
) -> Value {
    let mut object = response_object("error", request_id);
    object.insert("code".to_string(), Value::from(code));
    object.insert("message".to_string(), Value::from(message));
    Value::Object(object)
}

fn response_object(kind: &'static str, request_id: Option<Value>) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::from(kind));
    if let Some(request_id) = request_id {
        object.insert("requestId".to_string(), request_id);
    }
    object
}

fn serialize_value(value: impl Serialize) -> Value {
    serde_json::to_value(value).unwrap_or_else(|error| {
        log::error!("failed to encode API response payload: {error}");
        Value::Null
    })
}
