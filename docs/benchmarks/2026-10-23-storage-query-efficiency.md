# Storage query efficiency measurements (2026-10-23)

Bundled SQLite, fresh `library.db` from `Store::open`; plans captured by
`crates/store/tests/cleanup_index_work.rs` (`-- --nocapture`).

## Q1 reverse-reference cleanup indexes

| Statement / FK check | Before | After |
|---|---|---|
| `probe_stage_state WHERE active_job_id = ?` (data_retention job prune) | `SCAN probe_stage_state` | `SEARCH ... USING INDEX probe_stage_active_job` |
| `unidentified WHERE root_id = ?` (root removal) | `SCAN unidentified` | `SEARCH ... USING COVERING INDEX unidentified_root` |
| FK parent check `media(survivor_id)` on Media delete/re-key | `SCAN media` | `SEARCH ... USING INDEX media_survivor` |

Not added (no real statement filters by the key; FK checks already searched):
`fingerprint_attempts(file_id)`, `metadata_inputs(media_id)`, `library_operations_scope`,
playback logs/metrics user/media, search_snapshots. Write/size cost not measured;
both partial indexes cover only non-NULL rows.
