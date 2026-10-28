# Task 22 slice 1 — on-executor `Store` locks: playback and search

> Historical slice record, not a claim that every async Store wait is gone. Current code `9f6126d3` fixes the previously named scan and main up-next Store stages, but Media operation handlers and up-next artwork still block the executor; see [2026-10-28 review R8](2026-10-28-storage-main-rereview.md#r8执行器阻塞仍有两个生产分支). Wait budgets below describe this historical stage; current values live in `http/store_wait.rs`.

Audit and migration record for the user-latency paths. Task 22 covers ~174
on-executor `.store.lock()` sites across 72 files; this slice lands the two paths
where a parked tokio worker is felt by a user directly: **playback**
(`http/playback*.rs`, `http/playback/**`) and **search** (`http/search*.rs`,
`http/search/**`).

Method: `grep -rn '\.store\.lock()'` over the slice, then for each hit the
enclosing function and whether it already runs off the executor (inside
`spawn_blocking`, `state.http_work.run*`, `state.search_work.run`).

## The seam

`crates/api/src/http/store_wait.rs` is the single implementation, generalising
the Task 7 shape (`http/auth.rs::auth_store_lookup`,
`http/jobs*::with_bounded_store`):

| entry point | use |
|---|---|
| `store_read(state, work)` | a handler that is already `async`: the store body runs on the blocking pool, the closure waits `STORE_LOCK_WAIT` (1 s) for the mutex |
| `with_bounded_store(handle, work)` | code already off the executor (inside a `*_work.run` closure): bounded `try_lock_for`, `None` → busy |

Taxonomy (unchanged from Task 7): contended lock or elapsed deadline → **503
`runtime.busy` + `Retry-After: 1`**; a real read failure inside the closure keeps
the handler's own code (e.g. `store.error`, `playback.item_missing`); a blocking
worker panic → 500 `runtime.worker`. For a *write*, `Deadline` means "outcome
unknown" — the client retries an idempotent position/policy write.

`playback_views/` (favorites / gallery / up-next) is *not* matched by the slice
globs (`playback*.rs`, `playback/**`) and is left for slice 1b — it is four more
sites of the same shape.

## Slice 1 — playback and search (landed)

| # | site | enclosing fn | was | now |
|---|---|---|---|---|
| 1 | `playback.rs:162` | `item` | on-executor lock | `store_read` |
| 2 | `playback.rs:234` | `decide` | on-executor lock | `store_read` (whole body) |
| 3 | `playback.rs:274` | `metrics` | on-executor lock | `store_read` |
| 4 | `playback.rs:322` | `get_policy` | on-executor lock | `store_read` |
| 5 | `playback.rs:340` | `put_policy` | on-executor lock | `store_read` |
| 6 | `playback.rs:393` | `get_ui_prefs` | on-executor lock | `store_read` |
| 7 | `playback.rs:409` | `put_ui_prefs` | on-executor lock | `store_read` |
| 8 | `playback.rs:451` | `get_subtitle_file` | on-executor lock | `store_read` (store block only; delivery stays awaited outside) |
| 9 | `playback/chapter_cache.rs:22` | `cached_chapters` | on-executor lock | `store_read` |
| 10 | `playback/episode_listing.rs:24` | `episodes` | on-executor lock | `store_read` (store block; artwork already under `http_work`) |
| 11 | `playback/heartbeat.rs:58` | `progress` | on-executor lock | `store_read` (identity checks + row lookup) |
| 12 | `playback/heartbeat.rs:121` | `progress` | on-executor lock | `store_read` (position write) |
| 13 | `playback/session.rs:28` | `start_session` | on-executor lock | `store_read` |
| 14 | `playback/session.rs:137` | `issue_binding` | sync helper locking on the executor | async + `store_read` |
| 15 | `playback/session.rs:270` | `session_stream` | on-executor lock | `store_read`; busy → 503 |
| 16 | `playback/session.rs:288` | `session_chapters` | sync helper locking on the executor | async + `store_read`; busy degrades to "chapters pending" (`warn!`), because chapters must not fail the session grant |
| 17 | `playback/watch_state.rs:43` | `resume` | on-executor lock | `store_read` (whole body) |
| 18 | `playback/watch_state.rs:97` | `marks` | on-executor lock | `store_read` (whole body) |
| 19 | `playback/watch_state.rs:288` | `set_marks` | on-executor lock | `store_read` (whole body) |
| 20 | `playback_activity.rs:76` | `activity` | on-executor lock | `store_read` (whole body) |
| 21 | `playback_activity.rs:168` | `end_session` | on-executor lock | `store_read` (whole body) |
| 22 | `playback_activity.rs:199` | `devices` | on-executor lock | `store_read` (whole body) |
| 23 | `playback_activity.rs:271` | `revoke_device` | on-executor lock | `store_read` (whole body) |
| 24 | `playback_logs.rs:151` | `history` | on-executor lock | `store_read` (whole body) |
| 25 | `playback_logs.rs:310` | `clear_history` | on-executor lock | `store_read` (whole body) |
| 26 | `playback_stats.rs:46` | `watch_stats` | on-executor lock | `store_read` (whole body) |
| 27 | `search.rs:108` | `search_torrents` | on-executor lock | `store_read` |
| 28 | `search.rs:151` | `search_stream` | on-executor lock | `store_read` |
| 29 | `search.rs:353` | `search_library_items` | **already off-executor** (`search_work.run`), but the lock wait was unbounded | `with_bounded_store`; `None` → 503 `search.busy` |
| 30 | `search/history.rs:104` | `save_torrents_history` (sync helper called from async) | on-executor lock | async + `store_read`; busy keeps the best-effort contract (`false` = not recorded) |
| 31 | `search/history.rs:142` | `save_titles_history` (sync helper called from async) | on-executor lock | async + `store_read`, same |
| 32 | `search/history.rs:255` | `search_history` | on-executor lock | `store_read` |
| 33 | `search/history.rs:264` | `get_presets` | on-executor lock | `store_read` |
| 34 | `search/history.rs:281` | `put_presets` | on-executor lock | `store_read` |
| 35 | `search/history.rs:301` | `get_history_snapshot` | on-executor lock | `store_read` |
| 36 | `search/history.rs:342` | `delete_history_entry_route` | on-executor lock | `store_read` |
| 37 | `search/history.rs:365` | `clear_search_history_route` | on-executor lock | `store_read` |

Verified after the slice: `grep -rn '\.store\.lock()'` over
`http/playback*.rs`, `http/playback/**`, `http/search*.rs`, `http/search/**`
returns **0** hits.

## Deliberately not migrated (with reason)

| site | why it is not a defect |
|---|---|
| `playback/heartbeat.rs` → `crate::http::library::ensure_probe_enqueued` | the call chain enters `state.probe.request_probe`, which does not take `state.store.lock()`; the probe queue owns its own lock discipline (Task 0b/7) |
| `search.rs` → `fanout::collect_site_results` / `search_title_sources` | network fan-out already runs under the search budget; no store access on the executor |
| `playback.rs` → `state.browse_files` poster lookup | file IO already runs under `http_work.run` with its own deadline; no store lock |

## Slice 1b — enumerated, not landed

| site | enclosing fn | note |
|---|---|---|
| `playback_views/favorites.rs:56` | favorites read | `playback_views/` is outside the slice globs; same one-line `store_read` pattern |
| `playback_views/gallery.rs:49` | gallery read | same |
| `playback_views/up_next.rs:121` | `collect_plans` | same |
| `playback_views/up_next.rs:342` | up-next write path | same |

## Next slices (from the Task 22 audit, unchanged)

`library_artwork.rs` 13/13, `downloaders/instances.rs` 11/11, `library_chapters.rs`
10/8, `collections.rs` 9/9, `reidentify.rs` 9/2, `downloaders/submission.rs` 8/1,
`library.rs` 6/6, `rule_sets.rs` 6/6, `library_admin.rs` 6/4,
`library_housekeeping.rs` 6/4, `library_scan.rs` 13/4, then ~60 files with 1–5
each. Several belong to the paging/legacy owners' files and should ride with
their slices rather than a second agent.

The second class from the Task 22 description — background drivers started by
`ApiState::new` (notably `chapter_frame_service::worker_loop`) taking the lock on
the executor — is **not** in this slice.

## Slice 2 — `ApiState::new` drivers

Driver inventory and verdicts (source of truth: `ApiState::start_runtime`).

| driver | verdict |
|---|---|
| `chapter_frame_service::worker_loop` | **measured red then fixed**: on a current-thread runtime with a Store write held from another thread, an unrelated 50 ms ticker fired **0/20** before the fix and **20/20** after (`crates/api/tests/driver_executor_bounds.rs`). `fill_window` and `resolve_target` hop to the blocking pool, `finish_task` uses `store_wait` with the write budget. |
| `scan_probe_dispatch::start_dispatcher` | proven harmless by inspection: the loop hops to `spawn_blocking` for every store call and sleeps 5 s between rounds. |
| `root_reassign_work::start` | proven harmless by inspection: `fs_work.run_to_completion(‖ shared.lock().drain_one_root_batch())`. |
| `rename_work` | proven harmless by inspection: `spawn_blocking` per batch. |
| `probe_manager` metadata worker | **off the executor since slice 2b; NOT measured.** A gated `TracksProbe` stub (parks on demand, 10 s bound) controls the stage's lock window; the worker provably reaches the stage but never reaches its publish step, and a vacuity assertion catches it — both the post-2b and the pre-2b runs were green for the wrong reason. Diagnostic (task 32): **not** `observe_source` (returns `Ok`) and **not** `publish_probe_tracks` (never entered) — the stage's publish is reached through `store_wait` with `STORE_WRITE_WAIT` (1 s), and the instrument holds the lock for 2 s, so the bounded wait expires and the stage returns `Err` **by design**. The instrument therefore has to hold the lock for less than the budget it is testing; kept as `#[ignore]`d `probe_metadata_publish_window_is_unmeasurable_with_a_hold_longer_than_the_write_budget` with the recipe. |
| `probe_manager` fingerprint worker + fingerprint stages (`probe.rs`, `season.rs`, `dispatch.rs`) | **unmeasured**, and cannot be measured until a unit survives to its store step. No claim is made about them. |
| `state.rs::start_runtime` → `recover_orphaned_scans` | justified: sync startup, before any request is served. |
| `http::browser_settings::spawn_reconcile` | not in this slice's write scope (owned with the scan routes). |
