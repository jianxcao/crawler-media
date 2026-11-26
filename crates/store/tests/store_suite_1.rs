//! Consolidated integration test suite to reduce redundant linker artifacts.
#![allow(clippy::duplicate_mod, clippy::module_inception)]

#[path = "adopted_filter_facts.rs"]
mod adopted_filter_facts;
#[path = "app_write.rs"]
mod app_write;
#[path = "borrowed_store_scopes.rs"]
mod borrowed_store_scopes;
#[path = "cleanup_index_work.rs"]
mod cleanup_index_work;
#[path = "connection_policy.rs"]
mod connection_policy;
#[path = "episode_panorama_scoped.rs"]
mod episode_panorama_scoped;
#[path = "file_deletion_queue.rs"]
mod file_deletion_queue;
#[path = "file_page_coverage_work.rs"]
mod file_page_coverage_work;
#[path = "file_rename_cross_fs.rs"]
mod file_rename_cross_fs;
#[path = "file_rename_shared_artwork.rs"]
mod file_rename_shared_artwork;
#[path = "fingerprint_cache.rs"]
mod fingerprint_cache;
#[path = "fixture_publication_equivalence.rs"]
mod fixture_publication_equivalence;
#[path = "integrity_projection_reads.rs"]
mod integrity_projection_reads;
#[path = "ledger_presence.rs"]
mod ledger_presence;
#[path = "library_integrity_concurrency.rs"]
mod library_integrity_concurrency;
#[path = "library_integrity_report_cap.rs"]
mod library_integrity_report_cap;
#[path = "library_query_v1.rs"]
mod library_query_v1;
#[path = "library_reconfiguration_roots.rs"]
mod library_reconfiguration_roots;
#[path = "library_removal_recovery.rs"]
mod library_removal_recovery;
#[path = "library_scope.rs"]
mod library_scope;
#[path = "login_snapshot.rs"]
mod login_snapshot;
#[path = "media_merge_invalid_phase.rs"]
mod media_merge_invalid_phase;
#[path = "media_reassign.rs"]
mod media_reassign;
#[path = "missing_files.rs"]
mod missing_files;
#[path = "nfo_observer_snapshot.rs"]
mod nfo_observer_snapshot;
#[path = "operations_retention.rs"]
mod operations_retention;
#[path = "password_migration.rs"]
mod password_migration;
#[path = "placed_source_bind.rs"]
mod placed_source_bind;
#[path = "playback_compatibility.rs"]
mod playback_compatibility;
#[path = "playback_event_duration.rs"]
mod playback_event_duration;
#[path = "playback_identity_barrier.rs"]
mod playback_identity_barrier;
#[path = "playback_session_archive.rs"]
mod playback_session_archive;
#[path = "probe_analysis_dispatch.rs"]
mod probe_analysis_dispatch;
#[path = "probe_dispatch_pause.rs"]
mod probe_dispatch_pause;
#[path = "probe_stage_state.rs"]
mod probe_stage_state;
#[path = "protocol_plans.rs"]
mod protocol_plans;
#[path = "query_model_layout.rs"]
mod query_model_layout;
#[path = "query_model_protocol.rs"]
mod query_model_protocol;
#[path = "query_work.rs"]
mod query_work;
#[path = "reader_layout_budget.rs"]
mod reader_layout_budget;
#[path = "reader_pool_identity.rs"]
mod reader_pool_identity;
#[path = "reader_request_snapshot.rs"]
mod reader_request_snapshot;
#[path = "root_reassign_batched.rs"]
mod root_reassign_batched;
#[path = "root_unidentified.rs"]
mod root_unidentified;
#[path = "scan_recorded_keys.rs"]
mod scan_recorded_keys;
#[path = "schema_version_guard.rs"]
mod schema_version_guard;
#[path = "sql_result_bytes.rs"]
mod sql_result_bytes;
#[path = "storage_v1.rs"]
mod storage_v1;
#[path = "subscribe_library_scope.rs"]
mod subscribe_library_scope;
#[path = "subscribe_wanted_batch.rs"]
mod subscribe_wanted_batch;
#[path = "user_removal.rs"]
mod user_removal;
#[path = "watcher_presence_work.rs"]
mod watcher_presence_work;
#[path = "writer_rank_errors.rs"]
mod writer_rank_errors;

#[test]
fn every_top_level_store_test_file_is_registered() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let cargo_toml = std::fs::read_to_string(manifest_dir.join("Cargo.toml")).unwrap();
    let suites: String = (1..=4)
        .map(|i| {
            std::fs::read_to_string(manifest_dir.join(format!("tests/store_suite_{i}.rs"))).unwrap()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut missing = Vec::new();
    for entry in std::fs::read_dir(manifest_dir.join("tests")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy();
        if name.starts_with("store_suite_") {
            continue;
        }
        let toml_ref = format!("path = \"tests/{name}\"");
        let suite_ref = format!("#[path = \"{name}\"]");
        if !cargo_toml.contains(&toml_ref) && !suites.contains(&suite_ref) {
            missing.push(name.into_owned());
        }
    }
    assert!(
        missing.is_empty(),
        "Unregistered store integration test files (add to Cargo.toml or store_suite_1..4.rs): {missing:?}"
    );
}
