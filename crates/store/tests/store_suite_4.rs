//! Consolidated integration test suite to reduce redundant linker artifacts.
#![allow(clippy::duplicate_mod, clippy::module_inception)]

#[path = "borrowed_root_aliases.rs"]
mod borrowed_root_aliases;
#[path = "chapter_frame_tasks.rs"]
mod chapter_frame_tasks;
#[path = "configuration_result_cas.rs"]
mod configuration_result_cas;
#[path = "current_schema_contract.rs"]
mod current_schema_contract;
#[path = "file_coverage_page.rs"]
mod file_coverage_page;
#[path = "file_media_under.rs"]
mod file_media_under;
#[path = "file_rename.rs"]
mod file_rename;
#[path = "file_rename_receipt.rs"]
mod file_rename_receipt;
#[path = "fingerprint_attempts.rs"]
mod fingerprint_attempts;
#[path = "fingerprint_source_safety.rs"]
mod fingerprint_source_safety;
#[path = "integrity_guard_work.rs"]
mod integrity_guard_work;
#[path = "ledger_page.rs"]
mod ledger_page;
#[path = "library_integrity.rs"]
mod library_integrity;
#[path = "library_integrity_mutation_budget.rs"]
mod library_integrity_mutation_budget;
#[path = "library_query_plans.rs"]
mod library_query_plans;
#[path = "library_reconfiguration.rs"]
mod library_reconfiguration;
#[path = "library_removal_plans.rs"]
mod library_removal_plans;
#[path = "library_schema_audit.rs"]
mod library_schema_audit;
#[path = "library_search_merge.rs"]
mod library_search_merge;
#[path = "media_merge_conflicts.rs"]
mod media_merge_conflicts;
#[path = "media_merge_snapshots.rs"]
mod media_merge_snapshots;
#[path = "metadata_adoption_compose.rs"]
mod metadata_adoption_compose;
#[path = "nfo_export_work.rs"]
mod nfo_export_work;
#[path = "open.rs"]
mod open;
#[path = "page_validate.rs"]
mod page_validate;
#[path = "perf_baseline.rs"]
mod perf_baseline;
#[path = "playback_assertions.rs"]
mod playback_assertions;
#[path = "playback_event_binding.rs"]
mod playback_event_binding;
#[path = "playback_event_watch_time.rs"]
mod playback_event_watch_time;
#[path = "playback_scope.rs"]
mod playback_scope;
#[path = "playback_view_pages.rs"]
mod playback_view_pages;
#[path = "plugin_confirmed_effects.rs"]
mod plugin_confirmed_effects;
#[path = "plugin_quota.rs"]
mod plugin_quota;
#[path = "plugin_scoped_results.rs"]
mod plugin_scoped_results;
#[path = "plugin_staged_install.rs"]
mod plugin_staged_install;
#[path = "plugin_trust_keys.rs"]
mod plugin_trust_keys;
#[path = "probe_counter_transitions.rs"]
mod probe_counter_transitions;
#[path = "probe_publication.rs"]
mod probe_publication;
#[path = "protocol_index.rs"]
mod protocol_index;
#[path = "query_model_counts.rs"]
mod query_model_counts;
#[path = "query_model_facets.rs"]
mod query_model_facets;
#[path = "query_model_integrity_barrier.rs"]
mod query_model_integrity_barrier;
#[path = "query_model_projection.rs"]
mod query_model_projection;
#[path = "read_writer_independence.rs"]
mod read_writer_independence;
#[path = "reader_pool_admission.rs"]
mod reader_pool_admission;
#[path = "reader_request_cleanup.rs"]
mod reader_request_cleanup;
#[path = "root_inputs_prefix.rs"]
mod root_inputs_prefix;
#[path = "root_reconfiguration_metadata_inputs.rs"]
mod root_reconfiguration_metadata_inputs;
#[path = "scan_probe_obligations.rs"]
mod scan_probe_obligations;
#[path = "schema_redundant_indexes.rs"]
mod schema_redundant_indexes;
#[path = "source_probe_obligation.rs"]
mod source_probe_obligation;
#[path = "storage_unused_fields.rs"]
mod storage_unused_fields;
#[path = "subscribe_compensation.rs"]
mod subscribe_compensation;
#[path = "subscribe_timestamps.rs"]
mod subscribe_timestamps;
#[path = "unidentified_identity.rs"]
mod unidentified_identity;
#[path = "watch_aggregate_rollback.rs"]
mod watch_aggregate_rollback;
#[path = "writer_diagnostics.rs"]
mod writer_diagnostics;
