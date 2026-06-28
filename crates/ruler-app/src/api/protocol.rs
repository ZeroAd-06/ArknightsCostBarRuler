use serde::Deserialize;
use serde_json::Value;

use crate::{
    commands::UiCommand,
    ui_state::{parse_timer_input_frames, FrameDisplayMode},
};

#[derive(Clone, Debug, PartialEq)]
pub struct ClientRequest {
    pub request_id: Option<Value>,
    pub action: ClientAction,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClientAction {
    GetSnapshot,
    GetFrame { frame_id: u64 },
    SendCommand(UiCommand),
    CancelCalibration,
    Exit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestError {
    pub request_id: Option<Value>,
    pub code: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawClientRequest {
    #[serde(rename = "type")]
    request_type: String,
    request_id: Option<Value>,
    frame_id: Option<u64>,
    frames: Option<i32>,
    filename: Option<String>,
    old: Option<String>,
    new_base: Option<String>,
    display_mode: Option<String>,
    time: Option<String>,
    scale: Option<f32>,
    x: Option<i32>,
    y: Option<i32>,
}

pub fn parse_client_request(text: &str) -> Result<ClientRequest, RequestError> {
    let value = serde_json::from_str::<Value>(text).map_err(|error| RequestError {
        request_id: None,
        code: "invalid_json",
        message: format!("invalid JSON request: {error}"),
    })?;
    let request_id = value.get("requestId").cloned();
    let raw = serde_json::from_value::<RawClientRequest>(value).map_err(|error| RequestError {
        request_id,
        code: "invalid_request",
        message: format!("invalid websocket request: {error}"),
    })?;
    raw.into_request()
}

impl RawClientRequest {
    fn into_request(self) -> Result<ClientRequest, RequestError> {
        let request_id = self.request_id.clone();
        let action = match self.request_type.as_str() {
            "getSnapshot" => ClientAction::GetSnapshot,
            "getFrame" => ClientAction::GetFrame {
                frame_id: self.required_u64("frameId", self.frame_id)?,
            },
            "prepareCalibration" => ClientAction::SendCommand(UiCommand::PrepareCalibration),
            "startCalibration" => ClientAction::SendCommand(UiCommand::StartCalibration),
            "cancelCalibration" => ClientAction::CancelCalibration,
            "useProfile" => ClientAction::SendCommand(UiCommand::UseProfile {
                filename: self.required_text("filename", self.filename.clone())?,
            }),
            "renameProfile" => ClientAction::SendCommand(UiCommand::RenameProfile {
                old: self.required_text("old", self.old.clone())?,
                new_base: self.required_text("newBase", self.new_base.clone())?,
            }),
            "deleteProfile" => ClientAction::SendCommand(UiCommand::DeleteProfile {
                filename: self.required_text("filename", self.filename.clone())?,
            }),
            "setDisplayMode" => {
                let value = self.required_text("displayMode", self.display_mode.clone())?;
                let Some(mode) = FrameDisplayMode::from_api(value.as_str()) else {
                    return Err(self.error(
                        "invalid_request",
                        format!("displayMode must be one of 0_to_n-1, 0_to_n, 1_to_n; got {value}"),
                    ));
                };
                ClientAction::SendCommand(UiCommand::SetDisplayMode(mode))
            }
            "adjustTimer" => ClientAction::SendCommand(UiCommand::AdjustTimer {
                frames: self.required_i32("frames", self.frames)?,
            }),
            "setTimer" => ClientAction::SendCommand(UiCommand::SetTimer {
                frames: self.timer_target_frames()?,
            }),
            "resetTimer" => ClientAction::SendCommand(UiCommand::ResetTimer),
            "undoResetTimer" => ClientAction::SendCommand(UiCommand::UndoResetTimer),
            "toggleLapTimer" => ClientAction::SendCommand(UiCommand::ToggleLapTimer),
            "setOverlayScale" => ClientAction::SendCommand(UiCommand::SetOverlayScale(
                self.required_f32("scale", self.scale)?,
            )),
            "saveOverlayPlacement" => ClientAction::SendCommand(UiCommand::SaveOverlayPlacement {
                x: self.required_i32("x", self.x)?,
                y: self.required_i32("y", self.y)?,
            }),
            "exit" => ClientAction::Exit,
            other => {
                return Err(self.error(
                    "unknown_request",
                    format!("unknown websocket request type '{other}'"),
                ));
            }
        };

        Ok(ClientRequest { request_id, action })
    }

    fn required_text(
        &self,
        field: &'static str,
        value: Option<String>,
    ) -> Result<String, RequestError> {
        let value = value.ok_or_else(|| self.missing_field(field))?;
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(self.error(
                "invalid_request",
                format!("field '{field}' must not be empty"),
            ));
        }
        Ok(trimmed.to_string())
    }

    fn required_i32(&self, field: &'static str, value: Option<i32>) -> Result<i32, RequestError> {
        value.ok_or_else(|| self.missing_field(field))
    }

    fn timer_target_frames(&self) -> Result<i32, RequestError> {
        if let Some(frames) = self.frames {
            if frames < 0 {
                return Err(self.error(
                    "invalid_request",
                    format!("field 'frames' must be non-negative; got {frames}"),
                ));
            }
            return Ok(frames);
        }

        let time = self.required_text("time", self.time.clone())?;
        parse_timer_input_frames(time.as_str()).ok_or_else(|| {
            self.error(
                "invalid_request",
                "field 'time' must be XX:XX:XX or XX frames".to_string(),
            )
        })
    }

    fn required_u64(&self, field: &'static str, value: Option<u64>) -> Result<u64, RequestError> {
        value.ok_or_else(|| self.missing_field(field))
    }

    fn required_f32(&self, field: &'static str, value: Option<f32>) -> Result<f32, RequestError> {
        let value = value.ok_or_else(|| self.missing_field(field))?;
        if !value.is_finite() {
            return Err(self.error("invalid_request", format!("field '{field}' must be finite")));
        }
        Ok(value)
    }

    fn missing_field(&self, field: &'static str) -> RequestError {
        self.error(
            "invalid_request",
            format!("missing required field '{field}'"),
        )
    }

    fn error(&self, code: &'static str, message: String) -> RequestError {
        RequestError {
            request_id: self.request_id.clone(),
            code,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_client_request, ClientAction};
    use crate::{commands::UiCommand, ui_state::FrameDisplayMode};
    use serde_json::json;

    #[test]
    fn parses_snapshot_and_frame_requests() {
        let snapshot = parse_client_request(r#"{"type":"getSnapshot","requestId":"snap"}"#)
            .expect("valid snapshot request");
        assert_eq!(snapshot.request_id, Some(json!("snap")));
        assert_eq!(snapshot.action, ClientAction::GetSnapshot);

        let frame = parse_client_request(r#"{"type":"getFrame","requestId":"frame","frameId":42}"#)
            .expect("valid frame request");
        assert_eq!(frame.request_id, Some(json!("frame")));
        assert_eq!(frame.action, ClientAction::GetFrame { frame_id: 42 });
    }

    #[test]
    fn parses_control_commands() {
        let cases = [
            (
                r#"{"type":"prepareCalibration"}"#,
                ClientAction::SendCommand(UiCommand::PrepareCalibration),
            ),
            (
                r#"{"type":"startCalibration"}"#,
                ClientAction::SendCommand(UiCommand::StartCalibration),
            ),
            (
                r#"{"type":"useProfile","filename":"profile.json"}"#,
                ClientAction::SendCommand(UiCommand::UseProfile {
                    filename: "profile.json".to_string(),
                }),
            ),
            (
                r#"{"type":"renameProfile","old":"old.json","newBase":"new"}"#,
                ClientAction::SendCommand(UiCommand::RenameProfile {
                    old: "old.json".to_string(),
                    new_base: "new".to_string(),
                }),
            ),
            (
                r#"{"type":"deleteProfile","filename":"profile.json"}"#,
                ClientAction::SendCommand(UiCommand::DeleteProfile {
                    filename: "profile.json".to_string(),
                }),
            ),
            (
                r#"{"type":"setDisplayMode","displayMode":"0_to_n"}"#,
                ClientAction::SendCommand(UiCommand::SetDisplayMode(FrameDisplayMode::ZeroToN)),
            ),
            (
                r#"{"type":"adjustTimer","frames":-30}"#,
                ClientAction::SendCommand(UiCommand::AdjustTimer { frames: -30 }),
            ),
            (
                r#"{"type":"setTimer","frames":75}"#,
                ClientAction::SendCommand(UiCommand::SetTimer { frames: 75 }),
            ),
            (
                r#"{"type":"setTimer","time":"01:02:03"}"#,
                ClientAction::SendCommand(UiCommand::SetTimer { frames: 1_863 }),
            ),
            (
                r#"{"type":"resetTimer"}"#,
                ClientAction::SendCommand(UiCommand::ResetTimer),
            ),
            (
                r#"{"type":"undoResetTimer"}"#,
                ClientAction::SendCommand(UiCommand::UndoResetTimer),
            ),
            (
                r#"{"type":"toggleLapTimer"}"#,
                ClientAction::SendCommand(UiCommand::ToggleLapTimer),
            ),
            (
                r#"{"type":"setOverlayScale","scale":1.25}"#,
                ClientAction::SendCommand(UiCommand::SetOverlayScale(1.25)),
            ),
            (
                r#"{"type":"saveOverlayPlacement","x":11,"y":22}"#,
                ClientAction::SendCommand(UiCommand::SaveOverlayPlacement { x: 11, y: 22 }),
            ),
        ];

        for (text, expected) in cases {
            let request = parse_client_request(text).expect("valid control request");
            assert_eq!(request.action, expected);
        }
    }

    #[test]
    fn parses_state_actions_that_do_not_map_to_ui_command() {
        let cancel =
            parse_client_request(r#"{"type":"cancelCalibration"}"#).expect("valid cancel request");
        assert_eq!(cancel.action, ClientAction::CancelCalibration);

        let exit = parse_client_request(r#"{"type":"exit"}"#).expect("valid exit request");
        assert_eq!(exit.action, ClientAction::Exit);
    }

    #[test]
    fn parses_set_display_mode_when_mode_is_valid() {
        let request = parse_client_request(r#"{"type":"setDisplayMode","displayMode":"1_to_n"}"#)
            .expect("valid request");

        assert_eq!(
            request.action,
            ClientAction::SendCommand(UiCommand::SetDisplayMode(FrameDisplayMode::OneToN))
        );
    }

    #[test]
    fn rejects_empty_profile_filename() {
        let error = parse_client_request(r#"{"type":"useProfile","filename":" "}"#)
            .expect_err("empty profile should fail");

        assert_eq!(error.code, "invalid_request");
        assert_eq!(error.message, "field 'filename' must not be empty");
    }

    #[test]
    fn rejects_unknown_request_and_echoes_request_id() {
        let error = parse_client_request(r#"{"type":"bogus","requestId":"r1"}"#)
            .expect_err("unknown request should fail");

        assert_eq!(error.request_id, Some(json!("r1")));
        assert_eq!(error.code, "unknown_request");
        assert_eq!(error.message, "unknown websocket request type 'bogus'");
    }

    #[test]
    fn rejects_invalid_frame_id_and_echoes_request_id() {
        let error = parse_client_request(r#"{"type":"getFrame","requestId":"r2","frameId":-1}"#)
            .expect_err("negative frame id should fail");

        assert_eq!(error.request_id, Some(json!("r2")));
        assert_eq!(error.code, "invalid_request");
        assert!(error.message.contains("invalid websocket request"));
    }

    #[test]
    fn rejects_invalid_display_mode() {
        let error = parse_client_request(r#"{"type":"setDisplayMode","displayMode":"zero"}"#)
            .expect_err("invalid display mode should fail");

        assert_eq!(error.code, "invalid_request");
        assert_eq!(
            error.message,
            "displayMode must be one of 0_to_n-1, 0_to_n, 1_to_n; got zero"
        );
    }

    #[test]
    fn rejects_invalid_set_timer_payloads() {
        let negative = parse_client_request(r#"{"type":"setTimer","frames":-1}"#)
            .expect_err("negative absolute timer should fail");
        assert_eq!(negative.code, "invalid_request");
        assert_eq!(
            negative.message,
            "field 'frames' must be non-negative; got -1"
        );

        let invalid_time = parse_client_request(r#"{"type":"setTimer","time":"1:60:0"}"#)
            .expect_err("invalid timer text should fail");
        assert_eq!(invalid_time.code, "invalid_request");
        assert_eq!(
            invalid_time.message,
            "field 'time' must be XX:XX:XX or XX frames"
        );
    }
}
