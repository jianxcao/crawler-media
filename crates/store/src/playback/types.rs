use domain::{
    LedgerId, LibraryFile, MediaId, PhysicalSourceId, TimelineMode, TypedItemId, UnitKey,
    UnitScope, UserId,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlaybackTarget {
    pub media_id: MediaId,
    pub unit: UnitKey,
    pub file: LibraryFile,
    pub config_revision: i64,
    pub timeline: TimelineMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeStatus {
    Compatible,
    SourceChanged,
    NoProgress,
    AlreadyPlayed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedResume {
    pub target: PlaybackTarget,
    pub start_ms: i64,
    pub duration_ms: Option<i64>,
    pub audio_track: Option<String>,
    pub subtitle_track: Option<String>,
    pub status: ResumeStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackMutation {
    pub user_id: UserId,
    pub scope: UnitScope,
    pub played: Option<bool>,
    pub favorite: Option<bool>,
    pub expected_generation: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackEventType {
    Playing,
    Progress,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoundPlaybackEvent {
    pub user_id: UserId,
    pub device_id: String,
    pub session_id: String,
    pub generation: i64,
    pub event_sequence: Option<i64>,
    pub item: TypedItemId,
    pub source_id: PhysicalSourceId,
    pub source_version: String,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub audio_track: Option<String>,
    pub subtitle_track: Option<String>,
    pub paused: bool,
    pub rate_bps: i64,
    pub client: Option<String>,
    pub event_type: PlaybackEventType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventOutcome {
    Applied,
    IgnoredStale,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaMergeRequest {
    pub request_id: String,
    pub survivor: MediaId,
    pub loser: MediaId,
    pub survivor_revision: i64,
    pub loser_revision: i64,
    pub refresh_targets: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperationStatus {
    pub operation_id: String,
    pub phase: String,
    pub state: String,
    pub error: Option<String>,
}
