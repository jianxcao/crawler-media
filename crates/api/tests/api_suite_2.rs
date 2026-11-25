//! Consolidated integration test suite to reduce redundant linker artifacts.
#![allow(clippy::duplicate_mod, clippy::module_inception)]

#[path = "adaptive_fingerprint_capture.rs"]
mod adaptive_fingerprint_capture;
#[path = "blocking_budget.rs"]
mod blocking_budget;
#[path = "browser_runtime.rs"]
mod browser_runtime;
#[path = "chapter_frame_capacity.rs"]
mod chapter_frame_capacity;
#[path = "design_improvements.rs"]
mod design_improvements;
#[path = "duplicate_deletion_safety.rs"]
mod duplicate_deletion_safety;
#[path = "fingerprint_cache.rs"]
mod fingerprint_cache;
#[path = "image_cache.rs"]
mod image_cache;
#[path = "jellyfin_images.rs"]
mod jellyfin_images;
#[path = "jellyfin_query_error.rs"]
mod jellyfin_query_error;
#[path = "job_updates_bounded.rs"]
mod job_updates_bounded;
#[path = "jobs_writer_contention.rs"]
mod jobs_writer_contention;
#[path = "library_delete_reimport.rs"]
mod library_delete_reimport;
#[path = "library_read_auth_race.rs"]
mod library_read_auth_race;
#[path = "library_read_perf.rs"]
mod library_read_perf;
#[path = "library_removal_races.rs"]
mod library_removal_races;
#[path = "log_export.rs"]
mod log_export;
#[path = "maintenance_control.rs"]
mod maintenance_control;
#[path = "maintenance_sqlite.rs"]
mod maintenance_sqlite;
#[path = "media_poster_delivery.rs"]
mod media_poster_delivery;
#[path = "persistence.rs"]
mod persistence;
#[path = "playback_watch_time.rs"]
mod playback_watch_time;
#[path = "probe_whole_unit_guard.rs"]
mod probe_whole_unit_guard;
#[path = "query_http_perf.rs"]
mod query_http_perf;
#[path = "reidentify_preview.rs"]
mod reidentify_preview;
#[path = "request_admission.rs"]
mod request_admission;
#[path = "root_reassignment_runtime.rs"]
mod root_reassignment_runtime;
#[path = "scan_probe_recovery.rs"]
mod scan_probe_recovery;
#[path = "scan_video_bounded.rs"]
mod scan_video_bounded;
#[path = "season_analysis_schedule.rs"]
mod season_analysis_schedule;
#[path = "storage_api_contract.rs"]
mod storage_api_contract;
#[path = "storage_transfer_scope.rs"]
mod storage_transfer_scope;
#[path = "subscribe_fact_io_isolation.rs"]
mod subscribe_fact_io_isolation;
#[path = "subscription_latency.rs"]
mod subscription_latency;
#[path = "transfers_paged_and_strm.rs"]
mod transfers_paged_and_strm;
#[path = "up_next_local_first.rs"]
mod up_next_local_first;
#[path = "watch_media_events.rs"]
mod watch_media_events;
