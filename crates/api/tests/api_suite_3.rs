//! Consolidated integration test suite to reduce redundant linker artifacts.
#![allow(clippy::duplicate_mod, clippy::module_inception)]

#[path = "adopted_reads_catalog_independence.rs"]
mod adopted_reads_catalog_independence;
#[path = "bootstrap_credentials.rs"]
mod bootstrap_credentials;
#[path = "c2b_race.rs"]
mod c2b_race;
#[path = "claim_transfer_source.rs"]
mod claim_transfer_source;
#[path = "directory_atomic.rs"]
mod directory_atomic;
#[path = "episode_still_proxy.rs"]
mod episode_still_proxy;
#[path = "fingerprint_capture_profile.rs"]
mod fingerprint_capture_profile;
#[path = "integrity_projection_reads.rs"]
mod integrity_projection_reads;
#[path = "jellyfin_page_perf.rs"]
mod jellyfin_page_perf;
#[path = "jellyfin_sql_page.rs"]
mod jellyfin_sql_page;
#[path = "jobs_cleanup_recovery.rs"]
mod jobs_cleanup_recovery;
#[path = "ledger_paging.rs"]
mod ledger_paging;
#[path = "library_explicit_ownership.rs"]
mod library_explicit_ownership;
#[path = "library_read_composition.rs"]
mod library_read_composition;
#[path = "library_read_wal.rs"]
mod library_read_wal;
#[path = "library_removal_subscribes.rs"]
mod library_removal_subscribes;
#[path = "maintenance_acceptance.rs"]
mod maintenance_acceptance;
#[path = "maintenance_http.rs"]
mod maintenance_http;
#[path = "maintenance_write_gate.rs"]
mod maintenance_write_gate;
#[path = "media_server_list_off_lock.rs"]
mod media_server_list_off_lock;
#[path = "playback_file_assets.rs"]
mod playback_file_assets;
#[path = "polish_logos_douban_and_overview.rs"]
mod polish_logos_douban_and_overview;
#[path = "protocol_entity_reads.rs"]
mod protocol_entity_reads;
#[path = "query_page_materialization.rs"]
mod query_page_materialization;
#[path = "remediation_scale.rs"]
mod remediation_scale;
#[path = "request_observation.rs"]
mod request_observation;
#[path = "runtime_downloader.rs"]
mod runtime_downloader;
#[path = "scan_report.rs"]
mod scan_report;
#[path = "search_library_items.rs"]
mod search_library_items;
#[path = "sites_media_runtime.rs"]
mod sites_media_runtime;
#[path = "storage_operations_runtime.rs"]
mod storage_operations_runtime;
#[path = "storage_write_runtime.rs"]
mod storage_write_runtime;
#[path = "subscribe_lifecycle_projection.rs"]
mod subscribe_lifecycle_projection;
#[path = "subscriptions_create.rs"]
mod subscriptions_create;
#[path = "tv_episode_panorama.rs"]
mod tv_episode_panorama;
#[path = "user_patch_atomicity.rs"]
mod user_patch_atomicity;
#[path = "watch_presence.rs"]
mod watch_presence;
