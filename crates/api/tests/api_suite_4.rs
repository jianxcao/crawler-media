//! Consolidated integration test suite to reduce redundant linker artifacts.
#![allow(clippy::duplicate_mod, clippy::module_inception)]

#[path = "arrivals_runtime.rs"]
mod arrivals_runtime;
#[path = "bounded_work_acceptance.rs"]
mod bounded_work_acceptance;
#[path = "collections_api_reads.rs"]
mod collections_api_reads;
#[path = "facets_budget.rs"]
mod facets_budget;
#[path = "fingerprint_comparison_measurement.rs"]
mod fingerprint_comparison_measurement;
#[path = "jellyfin_coarse_routes.rs"]
mod jellyfin_coarse_routes;
#[path = "jellyfin_paged_metadata.rs"]
mod jellyfin_paged_metadata;
#[path = "jellyfin_stream_binding.rs"]
mod jellyfin_stream_binding;
#[path = "jobs_layout.rs"]
mod jobs_layout;
#[path = "library_artwork_deletion.rs"]
mod library_artwork_deletion;
#[path = "library_optional_probe_switches.rs"]
mod library_optional_probe_switches;
#[path = "library_read_concurrency.rs"]
mod library_read_concurrency;
#[path = "library_removal_cover_purge.rs"]
mod library_removal_cover_purge;
#[path = "library_safety_acceptance.rs"]
mod library_safety_acceptance;
#[path = "maintenance_config.rs"]
mod maintenance_config;
#[path = "maintenance_reporting.rs"]
mod maintenance_reporting;
#[path = "maintenance_writer_isolation.rs"]
mod maintenance_writer_isolation;
#[path = "metadata_language_operation.rs"]
mod metadata_language_operation;
#[path = "playback_history_page.rs"]
mod playback_history_page;
#[path = "poster_cache_capacity.rs"]
mod poster_cache_capacity;
#[path = "protocol_paging_totals.rs"]
mod protocol_paging_totals;
#[path = "read_paths_bounded.rs"]
mod read_paths_bounded;
#[path = "removal_contracts.rs"]
mod removal_contracts;
#[path = "review_remediation_downloader.rs"]
mod review_remediation_downloader;
#[path = "scan_budget_recovery.rs"]
mod scan_budget_recovery;
#[path = "scan_scope.rs"]
mod scan_scope;
#[path = "search_presets_and_categories.rs"]
mod search_presets_and_categories;
#[path = "source_observation_liveness.rs"]
mod source_observation_liveness;
#[path = "storage_subscription_runtime.rs"]
mod storage_subscription_runtime;
#[path = "studio_brands_contract.rs"]
mod studio_brands_contract;
#[path = "subscribe_search_keywords.rs"]
mod subscribe_search_keywords;
#[path = "sync_http_runtime.rs"]
mod sync_http_runtime;
#[path = "ui.rs"]
mod ui;
#[path = "users.rs"]
mod users;
#[path = "watch_strm_source.rs"]
mod watch_strm_source;
