# Code review brief — storage redesign review fixes

## What you are reviewing

Repo: `/Users/jianxcao/work/fun/crawler-media`, branch `feat/storage-redesign`.

**Review scope: ONLY the diff `review-base..HEAD` plus any uncommitted/untracked changes in the
working tree.** `review-base` is a tag on the commit where this piece of work started. Do NOT
review the rest of the branch's history — it is 312 pre-existing commits and is out of scope.

## Intent

This work executes the plan `docs/superpowers/plans/2026-10-26-storage-redesign-review-fixes.md`.
**Read that plan first.** It lists, per task, the claimed defect, its cited evidence
(`file:line`), and the acceptance criterion. Your job is to judge whether the diff actually
fixes those defects, correctly, without regressions — not to give generic style advice.

Also read `AGENTS.md` (repo rules, hard limits) and `CONTEXT.md` (mandatory vocabulary:
`Library`, `Torrent`, `Subscribe`, `Release`, `wash_cut`, "library ledger"). Words in
`CONTEXT.md` marked `_Avoid_` must not appear in new code or docs.

Starting state worth knowing: HEAD did **not compile**. The branch is a half-finished refactor
(`852fbc23` "make library.db the single source of truth") whose leftovers still referenced the
deleted `ledger`/`LedgerRow` model. Part of the diff is repairing that baseline.

## Verify, do not trust

Every claim in the plan and in the commit messages is a hypothesis. Check it against the code.
You may run `cargo test -p <crate>`, `node --test web/test/*.test.mjs`, `npx tsc --noEmit`,
`git grep`, `sqlite3` on a temp database, and `EXPLAIN QUERY PLAN`. Do not start a server, do not
call the network, do not modify files.

## Focus areas, in priority order

1. **ACL / ownership correctness (plan D1, Task 2).** A published file's Library must be read from
   its explicit `library_id` / `root_id`. No path-prefix inference, no default-Library fallback,
   for an already-published file. Hunt for any surviving production path that maps path → Library
   and can hand a file to the wrong Library (that is an access-control leak, not a cosmetic issue).
   `classify_new_file` may only classify *new* files.
2. **Layout integrity (plan D5 / Tasks 10/16/28, Review Focus 5).** The work landed three
   separately measured layout rounds (2→3→4→5); the final `LIBRARY_USER_VERSION` is 5.
   Existing older layouts must be rejected with a message naming
   `scripts/storage-cutover-rebuild.sh`, and no upgrade ladder,
   compat view, dual-read of an old column, or old settings key may appear anywhere. Check that
   the DDL and the new version constant actually agree, and that every claim about an index is
   backed by an `EXPLAIN QUERY PLAN` assertion in a test.
3. **Deadlock / starvation (plan L1–L6, Tasks 6–9).** Symptoms to hunt: the instance-lease
   heartbeat contending on the global `Arc<Mutex<Store>>`; a `parking_lot` guard held across
   `.await`; blocking store work on a tokio worker; a `seen`-style auxiliary lock held while
   taking the Store lock; `VACUUM` starving writers for 5s; a `tick` that runs the runner while
   holding the queue lock. Lock-order inversions and lock-held-across-callback patterns are
   blockers.
4. **Hot-query behaviour (plan P1–P7, Tasks 10/12/14).** Each added index must be justified and
   must not duplicate an existing one. No new full-table scan on a browse/detail/playback path.
   `WITHOUT ROWID` conversions must be correct for the primary key and row width. Cursor paging
   must not skip or repeat items, and `offset` must really be gone (400), not silently ignored.
5. **Test integrity.** Tests must assert observable behaviour at a public seam. Flag: a test
   weakened or deleted to make the suite pass; a deleted test whose behaviour still exists;
   an assertion on a private helper, SQL text, or mock call count; a race test that proves nothing
   (only `sleep`); an unbounded cross-thread wait that can hang CI; a test that needs the network,
   qBittorrent, or Chromium. "Deleted with a stated reason" is acceptable only if the reason holds
   in the code — verify it.
6. **Size and shape (hard limits, not suggestions).** `.rs` file ≤ 800 lines (soft 200);
   production fn ≤ 120 lines (soft 60); test fn ≤ 120 lines; `lib.rs` ≤ 800 lines (soft 150) and
   module tree + re-exports only. Flag any file the diff pushes over a limit, and flag splits
   that are just line-count shuffling rather than splits by responsibility.
7. **Observability.** Critical paths must emit structured `tracing` (`error!` for unexpected
   failure, `warn!` for recoverable degradation, `info!` for business lifecycle, `debug!` for
   parse/HTTP detail). A silently-swallowed error on a write/publication/heartbeat path is a
   finding.
8. **Public static routes.** `/posters/*`, `/fanart/*`, `/stills/*`, `/chapters/*`,
   `/libraries/{id}/cover` must stay on the public router (no auth) or web `<img>` breaks.

## Output format

1. **BLOCKERS** — correctness, data loss, ACL leak, deadlock, migration break. Each: `file:line`,
   what is wrong, the concrete failure it causes, and the minimal fix.
2. **SHOULD-FIX** — perf, robustness, test-quality, observability. Same shape.
3. **PLAN DEVIATIONS** — anything the plan's `Files:`/steps require that is missing, or done
   differently, without a stated reason in the commit message.
4. **NITS** — at most 10, one line each.
5. **CHECKLIST** — a table with one row per focus area 1–8: `PASS` / `FAIL` / `N-A` plus a
   one-line justification with evidence. If you found nothing in a category, say so explicitly
   rather than inventing a finding.

Rank by severity. Cite code. Do not restate the diff back to me.
