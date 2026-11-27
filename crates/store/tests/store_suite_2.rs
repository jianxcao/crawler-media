//! Consolidated integration test suite to reduce redundant linker artifacts.
#![allow(clippy::duplicate_mod, clippy::module_inception)]

#[path = "app_writer_scopes.rs"]
mod app_writer_scopes;
#[path = "browse_first_page_plans.rs"]
mod browse_first_page_plans;
#[path = "collection_order_errors.rs"]
mod collection_order_errors;
#[path = "continuation_projection_lifecycle.rs"]
mod continuation_projection_lifecycle;
#[path = "episode_state_work.rs"]
mod episode_state_work;
#[path = "file_deletion_source_barrier.rs"]
mod file_deletion_source_barrier;
#[path = "file_publication.rs"]
mod file_publication;
#[path = "file_rename_durable.rs"]
mod file_rename_durable;
#[path = "file_unit_markers.rs"]
mod file_unit_markers;
#[path = "fingerprint_model_identity_barrier.rs"]
mod fingerprint_model_identity_barrier;
#[path = "fresh_start.rs"]
mod fresh_start;
#[path = "last_played_page.rs"]
mod last_played_page;
#[path = "ledger_presence_work.rs"]
mod ledger_presence_work;
#[path = "library_integrity_facts.rs"]
mod library_integrity_facts;
#[path = "library_integrity_sparse.rs"]
mod library_integrity_sparse;
#[path = "library_read_pool.rs"]
mod library_read_pool;
#[path = "library_removal.rs"]
mod library_removal;
#[path = "library_removal_topology.rs"]
mod library_removal_topology;
#[path = "library_search.rs"]
mod library_search;
#[path = "media_alias_updates.rs"]
mod media_alias_updates;
#[path = "media_merge_orphans.rs"]
mod media_merge_orphans;
#[path = "media_retired_refs.rs"]
mod media_retired_refs;
#[path = "missing_probe_work.rs"]
mod missing_probe_work;
#[path = "nfo_scoped_inputs.rs"]
mod nfo_scoped_inputs;
#[path = "ownership_snapshot.rs"]
mod ownership_snapshot;
#[path = "path_key_normalization.rs"]
mod path_key_normalization;
#[path = "playable_episode_coverage.rs"]
mod playable_episode_coverage;
#[path = "playback_constraints_v1.rs"]
mod playback_constraints_v1;
#[path = "playback_event_sequence.rs"]
mod playback_event_sequence;
#[path = "playback_resume_v1.rs"]
mod playback_resume_v1;
#[path = "playback_session_ids.rs"]
mod playback_session_ids;
#[path = "plugin_command_targets.rs"]
mod plugin_command_targets;
#[path = "plugin_lifecycle_policy.rs"]
mod plugin_lifecycle_policy;
#[path = "plugin_read_queries.rs"]
mod plugin_read_queries;
#[path = "plugin_slice_a_r8.rs"]
mod plugin_slice_a_r8;
#[path = "plugin_state.rs"]
mod plugin_state;
#[path = "plugin_upgrade_uninstall.rs"]
mod plugin_upgrade_uninstall;
#[path = "probe_analysis_pagination.rs"]
mod probe_analysis_pagination;
#[path = "probe_dispatch_plans.rs"]
mod probe_dispatch_plans;
#[path = "protocol_entities.rs"]
mod protocol_entities;
#[path = "protocol_series_scalar_work.rs"]
mod protocol_series_scalar_work;
#[path = "query_model_locale.rs"]
mod query_model_locale;
#[path = "query_model_paging.rs"]
mod query_model_paging;
#[path = "query_model_shared_barrier.rs"]
mod query_model_shared_barrier;
#[path = "reader_lease_scope.rs"]
mod reader_lease_scope;
#[path = "reader_pool_open.rs"]
mod reader_pool_open;
#[path = "removal_explicit_ownership.rs"]
mod removal_explicit_ownership;
#[path = "root_reassignment_scope.rs"]
mod root_reassignment_scope;
#[path = "root_write_ownership.rs"]
mod root_write_ownership;
#[path = "scan_seen_work.rs"]
mod scan_seen_work;
#[path = "scoped_coverage.rs"]
mod scoped_coverage;
#[path = "storage_cleanup_work.rs"]
mod storage_cleanup_work;
#[path = "storage_work_bounds.rs"]
mod storage_work_bounds;
#[path = "strict_library_paths.rs"]
mod strict_library_paths;
#[path = "subscribe_merge_fence_windows.rs"]
mod subscribe_merge_fence_windows;
#[path = "terminal_operation_admission.rs"]
mod terminal_operation_admission;
#[path = "verify_library_files.rs"]
mod verify_library_files;
#[path = "write_transaction_contention.rs"]
mod write_transaction_contention;
