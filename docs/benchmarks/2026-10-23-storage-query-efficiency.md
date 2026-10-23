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
playback logs/metrics user/media, search_snapshots. Write/size cost (`scripts/bench/q1_cleanup_index_cost.py`, system SQLite, 100k probe_stage_state
rows with 10% active jobs + 100k unidentified rows across 20 roots):

| | insert 200k rows | DB after VACUUM | 400 reverse lookups |
|---|---|---|---|
| without the 3 indexes | 725 ms | 16.49 MB | 1642.5 ms |
| with them | 799 ms (+10%) | 18.28 MB (+11%) | 1.5 ms |

## Q2 batch-end user summary finalization

`crates/store/tests/publication_batch_work.rs` (release): 100-file batch onto an
existing TV card. Before: 0 users 40.7 ms, 40 users 240.3 ms (summary refreshed
per file). After: 0 users 39.5 ms, 40 users 41.4 ms (once per Media per batch).
Deterministic gate: per-thread `store::work_counters::user_summary_writes()` — old code 4000 writes (40 users × 100 files), new 40.
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

## Q3a — studio light projection (measured 2026-10-23)

Harness: `scripts/bench/q3a_studio_projection.py`. Synthetic data: 100k media, details_json about 3 KB, one work source row each. The harness walks every page of 200 items.

| Variant | Full walk | Per page | Bytes leaving SQLite |
|---|---|---|---|
| Before: details_json + normalized_json blobs, parsed client-side | 1614 ms | 3.23 ms | 604.6 MB |
| After: `json_type`/`json_extract` projection of 4 strings | 991 ms | 1.98 ms | 1.1 MB |

Implemented in `library_query/items/hydration.rs::fill_studio`. The precedence rule is unchanged: details first, then normalized `fields`. The client-side parse in the harness is Python; the Rust serde parse it replaces is a lower absolute cost, but the blob transfer saving does not depend on the language.
