pub mod chapter;
pub mod fingerprint;
pub mod matcher;
pub mod target;
pub mod types;

pub use chapter::{
    annotate_chapters, build_complete_timeline_chapters, classify_chapter_title, probe_chapters,
};
pub use fingerprint::{
    AudioFingerprint, ChromaprintEngine, FINGERPRINT_ALGORITHM_VERSION, FingerprintEngine,
    MAX_MATCH_DURATION_SECS, MIN_MATCH_DURATION_SECS, extract_audio_fingerprint,
    extract_audio_fingerprint_at, extract_audio_fingerprint_at_with, find_common_segment,
    find_common_segment_with,
};
pub use matcher::{
    match_episodes_fingerprints, match_episodes_fingerprints_with, match_episodes_outros,
    match_episodes_outros_with,
};
pub use target::{
    DEFAULT_PROBE_UA, ProbeTarget, active_probe_ua, read_strm_url, set_custom_probe_ua,
};
pub use types::{Chapter, ChapterMarker, CommonSegment, DetectedIntro, DetectedOutro, MarkerType};
