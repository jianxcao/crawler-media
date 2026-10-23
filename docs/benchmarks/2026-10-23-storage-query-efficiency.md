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

## Q2 batch-end user summary finalization

`crates/store/tests/publication_batch_work.rs` (release): 100-file batch onto an
existing TV card. Before: 0 users 40.7 ms, 40 users 240.3 ms (summary refreshed
per file). After: 0 users 39.5 ms, 40 users 41.4 ms (once per Media per batch).
Prepared-statement caching for hot statements was not measured separately; not changed.

## Q3a / Q3b / Q4 / Q5: baseline only, no code change

Existing result targets all green on 7bee561 (logs under /workspace/logs):
store library_query_filters, adopted_filter_facts, connection_policy,
library_removal_recovery, data_retention 26/0; api facets_regression,
jellyfin_sql_page, jellyfin_paged_metadata 49/0; media source_cache +
connection_policy 6/0; api release storage_contention 4/0.

- Q3a: `fill_studio` still reads `details_json` + work-scope `normalized_json` for each page Media.
  The cost is bounded by the page size. No cold-field page budget has been measured, so the projection was not added.
- Q3b / Q4: no per-request EQP/VM or person/genre JSON scan cost has been captured yet. Not measured, so nothing changed.
- Q5: storage_contention reports `read_scope: None`, which counts as **Not measured**. PRAGMA, connection memory budgets and deletion batching are unchanged.
