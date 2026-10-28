# Storage query efficiency measurements (2026-10-23)

历史阶段测量，数值保留原版本边界。后续历史记录见[10-24 复核](../audits/2026-10-24-storage-rereview.md)与[汇总报告](2026-10-23-storage-redesign-report.md)，现行问题和剩余优化见[2026-10-28 复审](../audits/2026-10-28-storage-main-rereview.md)。各阶段“未改”不代表后续版本仍未实现。

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

## 7bee561历史基线：Q3a/Q3b/Q4/Q5当时未修改（后续结果见下文）

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

## Q3b — merged fixed-bucket facet aggregation (measured 2026-10-23)

Harness: request-level `crates/api/tests/endpoint_perf.rs` (release, axum router in-process,
`PERF_SIZES=50000 PERF_MODES=movie PERF_ENDPOINTS=facets,items PERF_SAMPLES=7`).
Logs: /workspace/logs/q3b-before.log, q3b-after.log.

| `GET /libraries/{id}/facets` (tier all), 50k movies | statements | median | p95 | bytes |
|---|---|---|---|---|
| Before: one SELECT per fixed bucket | 26 | 428.5 ms | 488.1 ms | 1180 |
| After: one SELECT per dimension (filtered SUM + candidate MAX per bucket) | 11 | 237.0 ms | 265.1 ms | 1180 |

`items` unchanged (4.05 → 4.12 ms, noise). Equivalence: `facets_regression`
(new `fixed_buckets_keep_zero_counts_and_conditional_favorite`) passes on both old and new
code; `store library_query_filters` 8/0. Per-statement VM steps were not captured
(only wall time and statement count at request level).

## Q4历史阶段 — Jellyfin 协议分页（store 级，`crates/api/tests/jellyfin_page_perf.rs`）

方法：release，`protocol_item_page` 直接调用（不含 HTTP），单库电影，每形状 7 次取中位数/最大值。
根因（EXPLAIN QUERY PLAN）：选源子查询被规划到 `files_presence(library_id,presence_state)`，
每个候选行扫整库文件；且 `source_file_id` 在外层被展开两次。修复：选源在 LIMIT 之后、
`sourced AS MATERIALIZED`、状态列加一元 `+` 强制 `files_media_library`、修订号改 LEFT JOIN。

| size | shape | before median ms | after median ms |
|---|---|---|---|
| 5000 | first_page | 7246.1 | 290.1 |
| 5000 | deep_offset(40000,空页) | 6815.0 | 17.4 |
| 5000 | genre Drama | 1279.3 | 383.6 |
| 5000 | person | 195.4 | 206.8 |
| 5000 | search | 100.7 | 112.0 |
| 50000 | first_page | >20 min 未完成（中止） | 208.4 (p95 238.3) |
| 50000 | deep_offset(40000) | 未测 | 289.8 (p95 439.9) |
| 50000 | genre Drama | 未测 | 1269.6 (p95 1379.0) |
| 50000 | person | 未测 | 1945.7 (p95 2165.2) |
| 50000 | search | 未测 | 933.7 (p95 947.6) |

20k 单语句（插桩）：page 3904 ms → 40 ms。
注：5k 的"after"在中间版本（仅后置选源、未强制索引）测得；50k 为最终版本。
剩余：genre/person/search 随行数线性增长，成本在逐行解析 NFO JSON 谓词（count 与 page 各一次），未在本项处理。

### Q4b历史阶段 — 单次扫描得到页与总数（`COUNT(*) OVER ()`，页空才补跑 count）

release，`jellyfin_page_perf`，median ms（基线 = Q4 提交 68e43bc 的 50k 数）：

| size | first_page | deep_offset | genre | person | search |
|---|---|---|---|---|---|
| 5000 | 12.5 | 20.3 (空页) | 82.7 | 173.5 | 55.7 |
| 20000 | 52.4 | 100.8 (空页) | 325.7 | 598.3 | 181.6 |
| 50000 | 148.8 | 212.7 | 878.7 | 1741.0 | 531.5 |
| 50000 基线 | 208.4 | 289.8 | 1269.6 | 1945.7 | 933.7 |

仍线性：每行要算 `scoped` 的 NFO/采纳 JSON 并逐行求谓词；person 主要耗在请求 id → 名字的解析（扫描全部 NFO 演职员）。
近乎恒定的扩展性要求写入期维护的 term（jellyfin_genre/person_id 已在 CHECK 中但无写入者）与标题 FTS5，并处理 display_locale 变更的失效——未实现。

## Q4c — write-time protocol index (genre / person / search)

Card reduction now stores `jellyfin_genre` (`id:`/`name:` keys), `person_id`/`person_tmdb_id`
terms and `library_items.search_text` (casefolded, every stored locale). The page uses them as a
sound prefilter (`terms_value(term_kind,term_value,library_id,media_id)` index) and keeps the exact
locale-chained predicate on the candidates only. Store-level `protocol_item_page`, release, median
of 7 (ms); same harness as Q4/Q4b, same box.

| size | genre before→after | person before→after | search before→after |
|---|---|---|---|
| 5k | 82.7 → 21.3 | 173.5 → 2.5 | 55.7 → 2.8 |
| 20k | 325.7 → 85.4 | 598.3 → 3.2 | 181.6 → 21.6 |
| 50k | 878.7 → 221.0 | 1741.0 → 6.0 | 531.5 → 58.7 |

person is flat; search scans one column (linear, ~1.2 µs/row); genre remains proportional to
matches (Drama = 1/6 of the library → 8334 rows sorted and counted at 50k). Without the
`terms_value` index the IN-list scanned the whole term table (~20 ms × 7 statements at 20k).
FTS5 not used: the single-column `instr` already meets the target and keeps substring semantics.
