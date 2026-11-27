//! Consolidated integration test suite to reduce redundant linker artifacts.
#![allow(clippy::duplicate_mod, clippy::module_inception)]

#[path = "backup_consistency.rs"]
mod backup_consistency;
#[path = "chapter_frame_publication.rs"]
mod chapter_frame_publication;
#[path = "collections_reads.rs"]
mod collections_reads;
#[path = "continue_watching.rs"]
mod continue_watching;
#[path = "favorites_page.rs"]
mod favorites_page;
#[path = "file_demotion.rs"]
mod file_demotion;
#[path = "file_publication_context.rs"]
mod file_publication_context;
#[path = "file_rename_playback_summary.rs"]
mod file_rename_playback_summary;
#[path = "filter_codec_roundtrip.rs"]
mod filter_codec_roundtrip;
#[path = "fingerprint_samples.rs"]
mod fingerprint_samples;
#[path = "integrity_fact_write_barriers.rs"]
mod integrity_fact_write_barriers;
#[path = "ledger_cleanup.rs"]
mod ledger_cleanup;
#[path = "library_defaults.rs"]
mod library_defaults;
#[path = "library_integrity_media_pages.rs"]
mod library_integrity_media_pages;
#[path = "library_query_filters.rs"]
mod library_query_filters;
#[path = "library_read_snapshot.rs"]
mod library_read_snapshot;
#[path = "library_removal_cleanup.rs"]
mod library_removal_cleanup;
#[path = "library_roster_lifecycle.rs"]
mod library_roster_lifecycle;
#[path = "library_search_integrity_recovery.rs"]
mod library_search_integrity_recovery;
#[path = "media_lookup_indexes.rs"]
mod media_lookup_indexes;
#[path = "media_merge_recovery.rs"]
mod media_merge_recovery;
#[path = "metadata_adoption.rs"]
mod metadata_adoption;
#[path = "nfo_export_ownership.rs"]
mod nfo_export_ownership;
#[path = "nfo_scoped_inputs_layers.rs"]
mod nfo_scoped_inputs_layers;
#[path = "page_hydration_bounds.rs"]
mod page_hydration_bounds;
#[path = "pending_resubmission.rs"]
mod pending_resubmission;
#[path = "playback_archive_resume.rs"]
mod playback_archive_resume;
#[path = "playback_credentials.rs"]
mod playback_credentials;
#[path = "playback_event_tracks.rs"]
mod playback_event_tracks;
#[path = "playback_retention.rs"]
mod playback_retention;
#[path = "playback_storage_v1.rs"]
mod playback_storage_v1;
#[path = "plugin_commands.rs"]
mod plugin_commands;
#[path = "plugin_operations.rs"]
mod plugin_operations;
#[path = "plugin_records.rs"]
mod plugin_records;
#[path = "plugin_slice_a_r9.rs"]
mod plugin_slice_a_r9;
#[path = "plugin_tasks.rs"]
mod plugin_tasks;
#[path = "probe_backpressure_upgrade.rs"]
mod probe_backpressure_upgrade;
#[path = "probe_dispatch_windows.rs"]
mod probe_dispatch_windows;
#[path = "probe_obligation_scale.rs"]
mod probe_obligation_scale;
#[path = "protocol_episode_page_work.rs"]
mod protocol_episode_page_work;
#[path = "publish_republish_cost.rs"]
mod publish_republish_cost;
#[path = "query_model_mutations.rs"]
mod query_model_mutations;
#[path = "read_freshness_coverage.rs"]
mod read_freshness_coverage;
#[path = "reader_planner_epoch.rs"]
mod reader_planner_epoch;
#[path = "reader_request_admission.rs"]
mod reader_request_admission;
#[path = "rename_phases_lock_free.rs"]
mod rename_phases_lock_free;
#[path = "root_reassignment_state.rs"]
mod root_reassignment_state;
#[path = "scan_operations.rs"]
mod scan_operations;
#[path = "scanned_media_identity.rs"]
mod scanned_media_identity;
#[path = "search_page_work.rs"]
mod search_page_work;
#[path = "seed_defaults_atomicity.rs"]
mod seed_defaults_atomicity;
#[path = "site_maintenance_receipts.rs"]
mod site_maintenance_receipts;
#[path = "storage_invariants.rs"]
mod storage_invariants;
#[path = "studio_hydration_and_brands.rs"]
mod studio_hydration_and_brands;
#[path = "subscribe_retention.rs"]
mod subscribe_retention;
#[path = "transfer_settings.rs"]
mod transfer_settings;
#[path = "watch_aggregate.rs"]
mod watch_aggregate;
#[path = "writer_diagnostic_logs.rs"]
mod writer_diagnostic_logs;
