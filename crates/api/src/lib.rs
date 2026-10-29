#![allow(
    clippy::result_large_err,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::needless_borrow,
    clippy::items_after_test_module,
    clippy::unnecessary_to_owned
)]

pub mod compact_time;
pub mod file_stat_read;
mod http_fetcher;
pub mod password;
pub use http_fetcher::HttpFetcher;
mod air_gate;
mod backdrop_enrichment;
mod catalog_refresh;
mod check_in;
mod completion;
mod episode_still;
pub(crate) mod fanart_artwork;
mod instance_lease;
pub mod job_loop;
mod jobs_api;
pub mod library_statistics;
mod open_coverage;
mod pack_fallback;
pub(crate) mod scrape_config;
pub(crate) mod scrape_metadata;
pub mod scrape_store;
mod season_check;
mod selective_files;
pub mod ssdp;
mod subscribe_create;
mod watch_intake;
mod watch_ledger;
mod watch_scrape;
pub mod worker;
pub use scrape_store::ScrapeStoreExt;
#[doc(hidden)]
pub use worker::{page_is_full_and_irrelevant, search_keywords_for};
mod admit;
pub mod auto_resolve;
pub mod bootstrap_credentials;
pub mod catalog;
mod catalog_cache;
mod catalog_fanout;
pub mod chapter_frame_service;
mod claim;
mod delivery;
pub mod directory;
mod dynamic_downloader;
pub mod fingerprint_job;
pub mod http;
pub mod library_lifecycle;
pub mod library_presence;
/// Pure, testable reconciliation helper (depth limit 5, 100,000-entry budget,
/// symlinks never followed out of scope, `incomplete` reported honestly, and
/// the result is NOT an authorization decision).
pub mod library_reconciliation;
pub mod maintenance;
pub mod management;
pub mod marker_resolver;
pub mod media_posters;
pub mod media_server_provider;
pub mod metadata_service;
pub(crate) mod pending_reconcile;
pub mod playback_events;
mod poster;
mod poster_fetch;
pub(crate) mod probe_wait;
mod rename_work;
mod root_reassign_work;
pub mod runtime_browser;
mod runtime_downloader;
pub mod scan_operations;
pub mod scan_probe_dispatch;
pub mod settings_keys;
pub mod site_maintenance;
pub mod storage_operations;
pub mod store;
mod store_readers;
pub mod theintrodb;
pub mod ui;
pub mod user_removal;
pub mod visible_scope;
pub mod write_gate;
pub use poster_fetch::PosterFetch;
pub mod subscribe_library_read;
pub use subscribe_library_read::read_subscribe_library_facts;
pub mod anilist_http;
pub mod bangumi_http;
pub mod blocking_budget;
pub mod cli;
pub mod config;
pub mod douban_http;
pub mod fs_watcher;
#[cfg(test)]
mod fs_watcher_tests;
pub mod http_agent;
pub mod job_updates;
pub mod library_cover_service;
pub mod obscura_manager;
pub mod password_login;
mod playback_read;
pub mod probe_manager;
pub mod subtitle_delivery;
pub mod system_logs;
pub mod tmdb_http;
pub mod tvdb_http;
pub mod user_agent;

pub use config::ServerConfig;
pub use dynamic_downloader::DynamicDownloader;
pub use fs_watcher::spawn_fs_watcher;
pub use http::notify::notify_transport::NotifyTransport;
pub use job_loop::{spawn_job_loop, spawn_job_loop_with};
pub use management::{ApiState, router};
pub use runtime_downloader::{ChosenDownloader, DownloaderEnv, choose_downloader};
pub use store::{ResolvedResume, Store};
pub use subtitle_delivery::{SubtitleDelivery, SubtitleRequest, SubtitleServiceError};

pub(crate) mod library_read_work;
