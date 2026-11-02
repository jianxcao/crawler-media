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
mod torrent_file;
mod transmission;

pub use client::{Downloader, DownloaderError};
pub(crate) use http::agent_for_url;
pub use identity::{
    core_titles_compatible, has_sample_mismatch, release_units_agree, torrent_matches_snapshot,
};
pub use memory::MemoryDownloader;
pub(crate) use name_matching::{extract_name_tokens, size_matches};
pub use name_matching::{names_match, normalize_name};
pub use owned::{magnet_info_hash, ownership_tag, unproven};
pub use path_map::{PathMap, apply_maps, parse_path_maps};
pub use pool::DownloaderSet;
pub use qbit::{QbitConfig, QbitDownloader};
pub(crate) use selection::selection_drops_all_videos;
pub use snapshot::{CLAIMED_TAG, OWNED_TAG_PREFIX, TASK_TAG, TaskSnapshot, owned_tags_from};
pub use torrent_file::torrent_info_hash;
pub use transmission::{TransmissionConfig, TransmissionDownloader};
