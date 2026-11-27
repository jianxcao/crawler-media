//! Consolidated integration test suite to reduce redundant linker artifacts.
#![allow(clippy::duplicate_mod, clippy::module_inception)]

#[path = "adaptive_fingerprint_capture.rs"]
mod adaptive_fingerprint_capture;
#[path = "blocking_budget.rs"]
mod blocking_budget;
#[path = "boost_cleanup_safety.rs"]
mod boost_cleanup_safety;
#[path = "browser_runtime.rs"]
mod browser_runtime;
#[path = "chapter_frame_capacity.rs"]
mod chapter_frame_capacity;
#[path = "design_improvements.rs"]
mod design_improvements;
#[path = "download_service_remediation.rs"]
mod download_service_remediation;
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
#[path = "library_delete_reimport.rs"]
mod library_delete_reimport;
#[path = "library_read_auth_race.rs"]
mod library_read_auth_race;
#[path = "library_read_perf.rs"]
mod library_read_perf;
#[path = "library_removal_races.rs"]
mod library_removal_races;
#[path = "maintenance_control.rs"]
mod maintenance_control;
#[path = "maintenance_sqlite.rs"]
mod maintenance_sqlite;
#[path = "persistence.rs"]
mod persistence;
#[path = "playback_watch_time.rs"]
mod playback_watch_time;
#[path = "plugin_core_integration_remediation.rs"]
mod plugin_core_integration_remediation;
#[path = "plugin_core_slice_c_r2.rs"]
mod plugin_core_slice_c_r2;
#[path = "plugin_core_slice_c_r6.rs"]
mod plugin_core_slice_c_r6;
#[path = "plugin_core_slice_c_remediation.rs"]
mod plugin_core_slice_c_remediation;
#[path = "plugin_http_config_perms.rs"]
mod plugin_http_config_perms;
#[path = "plugin_http_site_ports.rs"]
mod plugin_http_site_ports;
#[path = "plugin_p1c_lifecycle.rs"]
mod plugin_p1c_lifecycle;
#[path = "plugin_registry_lifecycle.rs"]
mod plugin_registry_lifecycle;
#[path = "plugin_slice_d_r11.rs"]
mod plugin_slice_d_r11;
#[path = "plugin_slice_d_r7.rs"]
mod plugin_slice_d_r7;
#[path = "plugin_slice_e_r10.rs"]
mod plugin_slice_e_r10;
#[path = "plugin_ui_assets.rs"]
mod plugin_ui_assets;
#[path = "probe_whole_unit_guard.rs"]
mod probe_whole_unit_guard;
#[path = "query_http_perf.rs"]
mod query_http_perf;
#[path = "registry_snapshot_api.rs"]
mod registry_snapshot_api;
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
#[path = "subscribe_hooks_and_admit.rs"]
mod subscribe_hooks_and_admit;
#[path = "subscription_latency.rs"]
mod subscription_latency;
#[path = "transfers_paged_and_strm.rs"]
mod transfers_paged_and_strm;
#[path = "up_next_local_first.rs"]
mod up_next_local_first;
#[path = "watch_media_events.rs"]
mod watch_media_events;
