//! Strict identity and value validation before any Playback work is scheduled.
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HeartbeatInput {
    pub device_id: String,
    pub play_session_id: String,
    pub event_sequence: i64,
    pub event: Event,
    media_item_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    season_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    episode_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    position_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<i64>,
    audio_track: Option<String>,
    subtitle_track: Option<String>,
    paused: Option<bool>,
    rate: Option<f64>,
    client: Option<String>,
    device_name: Option<String>,
    client_version: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Event {
    Start,
    Progress,
    Stop,
}

impl HeartbeatInput {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.device_id.trim().is_empty()
            || self.play_session_id.trim().is_empty()
            || self.play_session_id.len() > 128
            || self.event_sequence < 0
        {
            return Err("device_id、签发的 play_session_id 与非负 event_sequence 为必填");
        }
        if self
            .rate
            .is_some_and(|rate| !rate.is_finite() || !(0.25..=4.0).contains(&rate))
        {
            return Err("播放倍率必须在 0.25 到 4.0 之间");
        }
        Ok(())
    }
}
