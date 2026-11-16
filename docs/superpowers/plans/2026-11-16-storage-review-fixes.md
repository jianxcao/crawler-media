# Storage Review Fixes Implementation Plan

**Goal:** Fix the three confirmed review regressions: Library removal replaying disk deletions, stale playback sequences overwriting progress, and stale track selections being rebound to another source.

**Architecture:** Preserve the existing independent reader pools and per-database writers. Removal fences suppress queued file work and bounded snapshot/root windows discard video and sidecar intents; playback validates sequence and resume-package compatibility inside the event transaction.

**Spec:** The three findings accepted in this chat; `AGENTS.md` and `docs/adr/0012-library-removal.md` govern removal and lock boundaries.

## Constraints

- Stay on the current branch; no push or PR.
- Keep filesystem observation outside database writer scopes.
- Tests exercise public Store operations or the API deletion worker's observable disk effects.
- Wrap all cargo commands with `scripts/with-build-lock.py`, use `-j4` / four test threads, and run the final workspace suite once with `--exclusive`, `-j8`, eight test threads and `--no-fail-fast`.

## Library removal and deletion replay

- [x] Add failing tests to `crates/store/tests/file_deletion_queue.rs` for removal cancelling only its file intents, immediate freeze admission, and rejected late enqueues for frozen files. Replace the old alias-retirement test that explicitly expected deletion after removal.
- [x] Update `crates/api/src/http/file_delete/tests.rs` to assert original bytes survive a removal followed by a retry, including a worker holding an already-read intent and a symlink root.
- [x] Run the targeted Store and API tests and confirm the failures are the expected persisted intent / removed bytes.
- [x] Cancel intents in bounded removal snapshot windows; reject new intents for frozen files and suppress existing frozen intents at worker admission. A missing formerly-owned file row is not permission to unlink.
- [x] Rerun queue, removal and API deletion tests. Document cancellation in ADR-0012. Close active deletion claims by root ownership, including workers whose live rows disappeared before freeze.

## Playback transaction correctness

- [x] Add public Store tests with the shared `playback_event_support` fixture: commit sequence 3 then 2 or duplicate 3; assert zero unit/session/log mutation, including stale Stop. Events without sequences retain existing protocol behavior.
- [x] Run red; add the sequence check inside `commit_playback_event` before activation or writes; run green.
- [x] Add public Store tests that remember tracks on source A, switch to source B, then send an event without selections; repeat for a track-version change. Test omission retaining compatible tracks and explicit new selections winning.
- [x] Run red; preserve omitted selections only when the old physical binding and track version match; run green.

- [x] Persist the sequence watermark on the issued session across archive/sweep; reject old events while allowing newer progress to resume. Cover transaction rollback, pending replacement sessions and repeated archive of the same logical session.

- [x] Fix main-review HTTP findings: source-specific duration on omitted-duration heartbeats, and Active-session resumption after idle sweep.
- [x] Preserve explicit Library ownership on deletion intents and rename leftovers across file deletion and nested roots of another MediaKind.

## Verification and commit

- [x] Perform independent review of the combined fixes and address deletion-claim, untracked-sidecar and archived-session gaps.
- [x] Run changed-crate suites (Store 1,131 passed; API 2,443 passed and three failures subsequently resolved by targeted reruns).
- [x] Final formatting check passed after all corrections.
- [x] Final Clippy completed with warnings denied.
- [x] Astra reviewed the effective branch against the specified main merge base, reviewed the fixes, and approved merging subject to final Clippy/workspace validation.
- [x] Run the final workspace suite once: 4,380 passed, 28 ignored, one fixture failed because it supplied 16 values after the session table gained its 17th nullable column. Explicit column lists fixed the fixture; its three-test target passed on rerun. No production behavior failures remain.
- [x] Commit the scoped fixes and regression tests on the current branch; report the commit and actual validation results.
