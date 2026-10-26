mod file_transfer;
mod frame_control;
pub use frame_control::extract_frame_controlled;
pub mod fingerprint;
pub mod latest;
pub mod markers;
mod naming;
mod nfo;
mod nfo_identity;
mod nfo_season;
mod watch_identity;
pub use nfo_identity::{NfoIdentity, parse_nfo_identity};
pub use watch_identity::read_watch_identity;
mod probe_target;
mod scrape;
pub mod subtitles;
mod tracks;
mod watch;
mod work_control;
pub use work_control::WorkControl;

pub use file_transfer::{
    hardlink_fallback_count, set_hardlink_fallback_copy, transfer_file, transfer_file_controlled,
};
pub use fingerprint::{CommonSegment, extract_audio_fingerprint, find_common_segment};
pub use latest::{WatchTier, played_last};
pub use marker::{
    Chapter, ChapterMarker, MarkerType, annotate_chapters, build_complete_timeline_chapters,
    classify_chapter_title, probe_chapters, probe_chapters_controlled,
};
pub use naming::{default_pattern, render_path, validate_pattern};
pub use nfo::{CastMember, NfoMeta, parse_nfo, read_nfo, write_nfo, write_streamdetails_into_nfo};
pub use nfo_season::{write_season_nfo, write_season_nfo_in_existing_dir};
pub use probe_target::{
    DEFAULT_PROBE_UA, ProbeTarget, active_probe_ua, read_strm_url, set_custom_probe_ua,
};
pub use scrape::{scrape_beside, scrape_directory};
pub use subtitles::{
    ControlledSubtitleRunner, DeliveryError, SubtitlePayload, SubtitleRunner, deliver_subtitle,
    deliver_subtitle_controlled, deliver_subtitle_with_source, extract_embedded_subtitle,
    find_subtitle_by_index, srt_to_vtt, subtitle_index,
};
pub use tracks::{
    AudioTrack, SubtitleTrack, Tracks, VideoTrack, classify_resolution, external_subtitle_tracks,
    probe_tracks, probe_tracks_and_duration, probe_tracks_and_duration_controlled,
};
pub use watch::{
    FileError, ScanIssue, ScanReport, ScanRootReport, ScanRootStatus, TransferredFile,
    Unidentified, VIDEO_EXTENSIONS, WatchJob, WatchKind, WatchOutcome, is_video_file, scan_watch,
    scan_watch_intake_controlled, scan_watch_reported,
};
pub use watch::{ScanChunk, ScanWalkBudget, ScanWalker};
mod frames;
mod quality_probe;
mod transfer_mode;
mod types;
pub use frames::{extract_frame, extract_frame_with_ffmpeg, probe_duration};
pub use quality_probe::{Ffprobe, MediaProbe};
pub use transfer_mode::resolve_mode;
pub use types::{FileQuality, LibraryError, TransferMode};
