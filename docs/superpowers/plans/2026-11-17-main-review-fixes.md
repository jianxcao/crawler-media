# Main Review Fixes Implementation Plan

**Goal:** Resolve review blockers against main `756444f7`, then obtain a fresh merge-readiness verdict. The user switched all final reviews from Astra to Sol after model service failures.

**Architecture:** Keep existing reader/writer boundaries and bounded HTTP workers. Refresh keyset windows from their origin using each new page's cursor; preserve request generation checks.

**Spec:** User's review-until-merge-ready request, `AGENTS.md`, ADR-0008 and ADR-0012. Remain on `feat/storage-redesign`; no push, merge, PR or issue. Keep remote reads at one with 500ms cooldown.

## Jellyfin single-item executor isolation

- [x] Add a public-provider regression to `crates/api/tests/media_server_list_off_lock.rs`: block subtitle observation with `FileStatRead`, run resolution on a current-thread runtime, and assert an unrelated async task progresses before observation is released.
- [x] Run the test red via the build wrapper. Move the whole `entry_snapshot` stage in `resolve_single_item` into existing `run_sync`, then retain async enrichment.
- [x] Run the provider target green (3 tests); Astra approved the fix. Full API suite remains in final validation.

## Library page refresh continuity

- [x] Add component tests in `web/test/library-detail-window.test.mjs` for inserting/removing an early Media after two pages have loaded, for both poster and gallery modes. Assert the exact contiguous refreshed window and subsequent append.
- [x] Run red (four expected failures). Rebuild the page chain sequentially from the current window origin using newly returned cursors; stop at the end and discard obsolete generations. All 12 component tests pass.
- [x] Run green, then web lint/typecheck/test/build (994 tests passed; lint has warnings but no errors). Astra approved the frontend fixes.

## Final validation

- [x] Correct all three Library-removal messages to explain orphan Media favorites/progress cleanup and preserved disk content. This is a copy-only correction against ADR-0012.
- [x] Fix source-specific resume refresh, canonical watched-state requests, displayed resume positions, and stale mark responses. Nine new behavior cases failed red then passed; 13 targeted tests green.
- [x] Fix presence-sidecar cleanup/removal race; both HTTP regressions failed red, all 26 related tests passed green. Astra approved the fix.
- [x] User explicitly accepts full STRM URLs including token parameters in internal logs; leave this behavior unchanged and exclude it from blockers.

- [x] Resolve further confirmed Sol blockers using public behavior tests: gallery recall includes its origin; changed file content clears old resolution; active Transfers retain a path reservation so presence verification cannot recover them prematurely.
- [x] Run formatting and Clippy with warnings denied; use the build wrapper for all build/test commands. Final checks exited 0 after all Sol fixes.
- [x] Run the final workspace suite with `--exclusive`, `-j8`, `--no-fail-fast`, and eight test threads: 4,390 passed, 0 failed, 28 ignored. An initial sandboxed run failed loopback binds; the same command passed after loopback access was granted. Later Sol fixes require the affected API/Store/Web suites again.
- [x] Obtain Sol's explicit merge-ready verdict for API, Store and Web, plus independent cross-review of all three new fixes and a separate final review against main. No remaining P0–P2.
- [x] Confirm the latest API/Store full-suite results: API 2,457 passed / 12 ignored, Store 1,140 passed / 12 ignored, both exited 0. Scoped fixes are ready for the required current-branch commit.

## Additional recovery and client gates

- [x] Recover interrupted pending Transfers with actual source identity and durable Probe work; confirm public HTTP regression red/green and review approval. Keep live Transfers reserved through completion/drop; three HTTP/recorder regressions cover live placement, abandoned recorder and rejected completion.
- [x] Run Jellyfin protocol tests, including final media-server release (41 passed), API release (15 targets, 253 passed), complete identity paging, source binding, actual subtitles/Range bytes and decoded images in the isolated live run. Run the manual ignored 50,000-record release measurement (passed).
- [x] Validate VidHub with generated local/HTTP STRM Movie/Episode playback, tracks/subtitles, chapter jump, seek, persisted progress, marks, admin/member isolation, 207-Episode/Season browsing, search, title sort and restart/reentry. The user explicitly accepts the recorded external-client limitation: offline search says “not found” and retries the unchanged query from cache; changing query after reconnection restores results. Core workflows are accepted; do not claim every VH contract passed.
- [x] Remove only this run's VidHub connection after explicit user authorization, preserve the existing emby connection, stop both loopback test servers, and record evidence in ignored test reports.

## Sol follow-up validation

- [x] Gallery recall: real component plus real `useWallRecall`/localStorage regression failed `1 != 501`; corrected absolute index and hook dependency; 995 Web tests, typecheck, lint and build pass.
- [x] Source replacement: public coverage/browse regression failed on retained 2160p; clear resolution on new content while preserving same-bytes binding; 37 targeted tests and Store Clippy pass.
- [x] Active Transfer recovery: gated public HTTP verify test failed while Transfer retained ownership; separate Transfer path registry and nonblocking presence claim fix it; API lib 315 and 17 related integration tests pass.
- [x] Final API and Store full suites after these fixes; final formatting/Clippy; Jellyfin release API 253 and media-server 41 passed. All automated gates required for the current-branch commit passed.
