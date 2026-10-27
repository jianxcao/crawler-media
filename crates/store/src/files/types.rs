use domain::{
    LedgerId, LibraryId, MediaId,
    MediaKind,
};

#[derive(Clone, Debug, PartialEq)]
pub struct FileDetails {
    pub source_revision: i64,
    pub source_version: String,
    pub tracks_version: i64,
    pub video_json: Option<String>,
    pub audio_json: String,
    pub subtitle_json: String,
    pub chapters_json: Option<String>,
    pub fingerprint_json: Option<String>,
    pub outro_fingerprint_json: Option<String>,
    pub format_duration_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LibraryItemSummary {
    pub library_id: LibraryId,
    pub media_id: MediaId,
    pub kind: MediaKind,
    pub title: String,
    pub file_count: usize,
    pub available_file_count: usize,
    pub missing_file_count: usize,
    pub unknown_file_count: usize,
    pub distinct_episode_count: usize,
    pub distinct_season_count: usize,
    pub preferred_file_id: Option<LedgerId>,
    pub size_bytes: Option<u64>,
}
