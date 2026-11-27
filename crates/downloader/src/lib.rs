mod client;
mod http;
mod identity;
mod memory;
mod name_matching;
mod owned;
mod path_map;
mod pool;
mod qbit;
mod selection;
mod snapshot;
mod submit;
mod torrent_fetch;
mod torrent_file;
mod transmission;

pub use client::{
    ClientSubmitResult, ClientTaskQuery, DiskSpaceInfo, DiskSpaceStatus, Downloader,
    DownloaderError, SubmitOptions, SubmitPayload, TaskFileSet,
};
pub(crate) use http::agent_for_url;
pub use identity::{
    core_titles_compatible, has_sample_mismatch, release_units_agree, torrent_matches_snapshot,
};
pub use memory::{MemoryDownloader, MemoryTaskMetrics};
pub(crate) use name_matching::{extract_name_tokens, size_matches};
pub use name_matching::{names_match, normalize_name};
pub use owned::{magnet_info_hash, ownership_tag, unproven};
pub use path_map::{PathMap, apply_maps, parse_path_maps};
pub use pool::DownloaderSet;
pub use qbit::{QbitConfig, QbitDownloader};
pub(crate) use selection::selection_drops_all_videos;
pub(crate) use snapshot::is_instance_owned_tag;
pub use snapshot::{
    CLAIMED_TAG, OWNED_TAG_PREFIX, TASK_TAG, TaskSnapshot, is_reserved_system_tag, owned_tags_from,
    user_tags_from, validate_category_name,
};
pub use submit::{
    DownloadOpContext, DownloadOutcome, DownloadSubmitError, DownloadSubmitter, FrozenDownloadPlan,
};
pub use torrent_fetch::{
    PreparedTorrent, SubmissionPurpose, TorrentFetchError, TorrentHttpSource, TorrentSource,
    fetch_verified_torrent, prepare_download_payload, torrent_target_policy, with_torrent_limits,
};
pub use torrent_file::{
    MAX_TORRENT_BYTES, TorrentParseError, VerifiedTorrentPayload, parse_verified_torrent,
    torrent_info_hash,
};
pub use transmission::{TransmissionConfig, TransmissionDownloader};
