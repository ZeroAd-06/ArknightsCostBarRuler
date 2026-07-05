use serde::Serialize;

use crate::{
    ui_state::{ApiFrameRecord, ApiHistoryBounds, ProfileMenuItem, VERSION},
    worker::AppStateSnapshot,
};

pub const API_VERSION: u32 = 2;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiPayload {
    pub api_version: u32,
    pub app_version: &'static str,
    pub is_running: bool,
    pub current_frame: Option<i32>,
    pub total_frames_in_cycle: i32,
    pub total_elapsed_frames: i32,
    pub active_profile: Option<String>,
    pub frame_id: Option<u64>,
    pub sample_index: u64,
    pub dropped_since_previous: u64,
    pub raw_pixel_width: Option<i32>,
    pub cost_is_negative: bool,
    pub battle_state: Option<String>,
    pub capture_width: Option<u32>,
    pub capture_height: Option<u32>,
    pub capture_format: Option<String>,
    pub capture_timestamp_ns: Option<u64>,
    pub capture_duration_us: Option<u64>,
    pub cursor_blocked: bool,
    pub display_mode: &'static str,
    pub display_frame: String,
    pub display_total: String,
    pub time: String,
    pub lap_frames: Option<i32>,
    pub can_undo_reset: bool,
    pub profiles: Vec<ApiProfilePayload>,
    pub history_oldest_frame_id: Option<u64>,
    pub history_latest_frame_id: Option<u64>,
    pub timing_debug: Option<TimingDebugPayload>,
}

impl ApiPayload {
    #[must_use]
    pub fn from_snapshot(snapshot: &AppStateSnapshot, bounds: ApiHistoryBounds) -> Self {
        let api = &snapshot.api;
        let ui = &snapshot.ui;
        Self {
            api_version: API_VERSION,
            app_version: VERSION,
            is_running: api.is_running,
            current_frame: api.current_frame,
            total_frames_in_cycle: api.total_frames_in_cycle,
            total_elapsed_frames: api.total_elapsed_frames,
            active_profile: api.active_profile.clone(),
            frame_id: api.frame_id,
            sample_index: api.sample_index,
            dropped_since_previous: api.dropped_since_previous,
            raw_pixel_width: api.raw_pixel_width,
            cost_is_negative: api.cost_is_negative,
            battle_state: api.battle_state.clone(),
            capture_width: api.capture_width,
            capture_height: api.capture_height,
            capture_format: api.capture_format.clone(),
            capture_timestamp_ns: api.capture_timestamp_ns,
            capture_duration_us: api.capture_duration_us,
            cursor_blocked: ui.cursor_blocked,
            display_mode: ui.display_mode.as_config(),
            display_frame: ui.display_frame.clone(),
            display_total: ui.display_total.clone(),
            time: ui.time_str.clone(),
            lap_frames: ui.lap_frames,
            can_undo_reset: ui.can_undo_reset,
            profiles: ui
                .profiles
                .iter()
                .map(ApiProfilePayload::from_profile)
                .collect(),
            history_oldest_frame_id: bounds.oldest_frame_id,
            history_latest_frame_id: bounds.latest_frame_id,
            timing_debug: api.timing_debug.map(TimingDebugPayload::from_debug),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiFramePayload {
    pub frame_id: u64,
    pub sample_index: u64,
    pub dropped_since_previous: u64,
    pub is_running: bool,
    pub current_frame: Option<i32>,
    pub total_frames_in_cycle: i32,
    pub total_elapsed_frames: i32,
    pub active_profile: Option<String>,
    pub raw_pixel_width: Option<i32>,
    pub cost_is_negative: bool,
    pub battle_state: String,
    pub capture_width: u32,
    pub capture_height: u32,
    pub capture_format: String,
    pub capture_timestamp_ns: u64,
    pub capture_duration_us: u64,
    pub timing_debug: Option<TimingDebugPayload>,
}

impl ApiFramePayload {
    #[must_use]
    pub fn from_record(record: &ApiFrameRecord) -> Self {
        Self {
            frame_id: record.frame_id,
            sample_index: record.sample_index,
            dropped_since_previous: record.dropped_since_previous,
            is_running: record.is_running,
            current_frame: record.current_frame,
            total_frames_in_cycle: record.total_frames_in_cycle,
            total_elapsed_frames: record.total_elapsed_frames,
            active_profile: record.active_profile.clone(),
            raw_pixel_width: record.raw_pixel_width,
            cost_is_negative: record.cost_is_negative,
            battle_state: record.battle_state.clone(),
            capture_width: record.capture_width,
            capture_height: record.capture_height,
            capture_format: record.capture_format.clone(),
            capture_timestamp_ns: record.capture_timestamp_ns,
            capture_duration_us: record.capture_duration_us,
            timing_debug: record.timing_debug.map(TimingDebugPayload::from_debug),
        }
    }
}

/// Fixed-point fp24 timing diagnostics for one frame, mirroring the debug CSV.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingDebugPayload {
    pub required_fp: i64,
    pub speed_fp: i64,
    pub accumulator_fp: i64,
    pub advanced_frames: i32,
    pub frames_since_cycle_start: i32,
    pub frames_until_next_cost: i32,
    pub match_error_px: i32,
    pub boundary_corrected: bool,
}

impl TimingDebugPayload {
    fn from_debug(debug: ruler_core::TimingDebug) -> Self {
        Self {
            required_fp: debug.required_fp,
            speed_fp: debug.speed_fp,
            accumulator_fp: debug.accumulator_fp,
            advanced_frames: debug.advanced_frames,
            frames_since_cycle_start: debug.frames_since_cycle_start,
            frames_until_next_cost: debug.frames_until_next_cost,
            match_error_px: debug.match_error_px,
            boundary_corrected: debug.boundary_corrected,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiProfilePayload {
    pub filename: String,
    pub basename: String,
    pub total_frames: String,
    pub resolution: String,
    pub is_active: bool,
}

impl ApiProfilePayload {
    fn from_profile(profile: &ProfileMenuItem) -> Self {
        Self {
            filename: profile.filename.clone(),
            basename: profile.basename.clone(),
            total_frames: profile.total_frames_str.clone(),
            resolution: profile.resolution.clone(),
            is_active: profile.is_active,
        }
    }
}
