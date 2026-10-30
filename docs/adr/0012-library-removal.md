# ADR-0012: Library removal preserves files and purges orphan Media

- Status: Accepted design; implementation acceptance is tracked separately below.
- Scope: approved Library deletion and NFO reimport plan, Tasks 1–7.
- Current-layout note (2026-10-20): the design/history below uses “ledger” and `file_meta` terminology from its implementation period. Current persistent file facts are `library_files`, derived probe details are `file_details`, and Media/Playback now share `library.db`; `app.db`, `cache.db`, `subscribe.db` are separate Store files, with `jobs.db` outside Store. There is still no cross-file atomic transaction. Source: `crates/store/src/open.rs`, `schema/v1/library.sql`, `ledger_cleanup.rs`. Current findings and fix status are in the [storage lock review](../audits/2026-10-28-storage-lock-review.md), not inferred from this ADR.

## Context

A Library configuration, its ledger/file-derived facts, a canonical Media, and a User's Playback history have different lifetimes. Removing a configured root must not mean deleting disk content. (Superseded for orphan Media: see Decision — Media left without any file and not user-curated are purged with their Playback.) Recreating a root creates a new Library and new ledger rows; matching only a filename or reusing an old path is not sufficient to recover identity safely.

Store uses four independent SQLite connections/files, plus a separate Jobs queue connection/file. An ordinary transaction on one connection does not atomically commit the others. Cancellation notification alone also does not prove an old worker has stopped publishing: blocking scans and child processes can outlive the requesting future.

## Decision

### Remove indexing facts, not disk content or user history

Removal deletes only the strict ownership snapshot's ledger and derived file metadata, fingerprint caches/samples/attempts and probe-stage state. When a Media loses its last Library file and is not kept by a collection entry or a subscription, it is purged together with every row it owns (aliases, metadata, episodes, markers, probe/fingerprint state and each User's Playback position, watched state and favorite state) — the same rule as single-file delete (`library_write/orphan_media.rs`). Media still backed by another Library's file or kept by a collection/subscription keep their Playback facts. Historic logs (`playback_logs`, `playback_metrics`, removal receipts), Subscribe, Downloader configuration and Catalog caches are retained. Video, STRM, subtitles, NFO, poster, fanart and episode artwork are neither removed, moved nor rewritten by this operation.

Ownership is captured before roots disappear, using the longest uniquely matching root and directory-component boundaries. Default-library fallback is not deletion ownership. Ambiguous roots fail preflight rather than guessing. Nested independent libraries and another Library's file for the same Media remain intact. Shared Media/episode facts cannot be indiscriminately cleared while another ledger still needs them.

The last Library of a kind remains protected under the existing default-Library/Transfer contract. Removing a non-last default promotes an existing Library only at finalization. Supporting zero libraries of a kind is a separate change.

### Durable operation and publication barriers

Admin `DELETE /api/v1/libraries/{id}` returns `202` with an operation identifier, including when cleanup is fast. Admin `GET /api/v1/library-removals/{operation_id}` exposes `frozen`, `draining`, `cleaning`, `succeeded` or `failed`, the snapshot removal count and any error. Repeated requests use the existing operation.

The sequence (since `ffa96c36`, 2026-10-29) is freeze/snapshot → prevent new work → cancel all queued/running work of the Library → delete immediately → idempotent scoped cleanup → delete root configuration → succeeded. Removal does **not** wait for in-flight scan/probe/marker/scrape work or leases to drain: cancellation returns only after every publication gate is closed, so a late publication (covers included) is refused, and the Store refuses any write into a Library under removal (`library_removals` fence; FKs once the row is gone). Canceled work notices the cancellation on its own and its result is discarded (`crates/api/src/library_lifecycle/removal.rs`). The earlier lease-drain wait and its 30s re-cancel slices were removed.

The durable operation stores the cleanup scope so a restart or partial cross-database cleanup can resume safely. It does not pretend the four databases share a transaction. Cleanup retries must preserve the snapshot count and not revive canceled work. Frozen libraries are hidden from ordinary browsing/new Playback and remain visible for administrative status/retry. Static artwork read endpoints remain public under the existing image-authentication contract.

After Library finalization commits, explicit Subscribe bindings to that Library are cleared in a separate IMMEDIATE transaction on subscribe.db. The Subscribe itself is retained and subsequent Transfer resolves the current default Library of the same MediaKind. If that transaction fails or the process stops between commits, the durable succeeded removal receipt drives idempotent detachment on retry or startup; cleanup never acquires write locks on both databases together.

The durable succeeded receipt records Library finalization. Public removal status and repeated DELETE responses continue to report `cleaning` while Subscribe bindings to that Library remain, and expose `succeeded` only after the independent detachment commit is visible. This uses the existing states and does not rewrite the durable receipt during recovery.

### Reimport identity comes from reliable work-level aliases

Reimport creates new ledger identifiers and reuses an existing MediaId only when the work identity is reliable. External aliases include MediaKind: equal numeric TMDB movie and TV identifiers do not merge works. Conflicting provider aliases must fail rather than choose an arbitrary Media.

Movie identity comes from the matching movie NFO. TV identity comes from bounded ancestor `tvshow.nfo` lookup inside the Library root. An `episodedetails` NFO can supply season/episode facts, but its episode title/identifier is not a TV Media identity. Valid NFO identity and display metadata take precedence over a confident filename. Title-only fallback is allowed only when unique and non-conflicting; absent or ambiguous identifiers do not guarantee history recovery. Old history is retained even when it cannot safely be associated with a newly scanned file.

A complete local identity plus display title avoids implicit Catalog search/details and existing-sidecar rewrites; an explicit refresh may still fetch fresh metadata. A later online identification must converge the new ledger on the canonical MediaId before publication rather than attach the same alias to a competing Media.

NFO metadata is not stream-probe output. Reimport must schedule fresh media probing for the new ledger rather than copy old file_meta or infer probe completion from NFO runtime/streamdetails. New libraries keep `detect_intros=false` and `enable_fingerprint=false`; these switches do not disable basic media probing.

## Consequences and acceptance boundaries

- Reliable MediaId reuse reconnects Playback facts only for Media that survived removal (kept by another file, collection or subscription); purged orphan Media come back as new Media without old favorites/progress.
- Files stay available to external tools throughout removal. The application must stop offering new Playback through removed ledger rows.
- Removal no longer blocks on slow work; a removal that fails in cleanup may be retried and stays frozen until it succeeds.
- No path-cache migration, unowned-ledger adoption or history-clearing option is introduced.

The public API regression source in `crates/api/tests/library_delete_reimport.rs` contains cases for TV S01E03 positions 750000/120000 ms with distinct favorite/watched states, whole-movie positions, fresh ledger identity, old file-meta/fingerprint/stage cleanup, preserved sidecar bytes, an independent Library's same-Media file/cache, default-off detection switches, and a different-alias negative case. These describe assertions, **not a fresh pass result**. It constructs ApiState before entering Tokio so probe workers are not started, and injects an offline Catalog. This checks cache non-reuse, **not** successful fresh probe execution, real streaming or child-process cancellation. Creation also schedules an automatic scan; the explicit public scan is intentionally idempotent with that path. Under the orphan-purge rule, history recovery in those cases only holds where the Media is kept by another Library's file, a collection or a subscription.

Restart-mid-clean, canceled-job recovery, lease/publication races, ambiguous no-ID reimport, nested-root ownership, member visibility, Subscribe-target handling and actual fake-probe completion require the corresponding focused suites and/or UI regression cases. This ADR is not proof those checks passed. Browser/VidHub cases remain Not run unless actually executed.

## External convention references (limited claim)

The approved plan cites these user-facing removal discussions:

- [Jellyfin: delete a library without deleting files](https://forum.jellyfin.org/t-delete-a-library-without-deleting-files?pid=71881&mode=linear)
- [Emby: delete library](https://emby.media/community/topic/117044-delete-library/)

They are reference leads for the narrow distinction between removing a library configuration and deleting media files, not evidence about either project's internal caches, cancellation or Playback identity retention. During this implementation both URLs could not be fetched because the tool resolved their hosts to non-public addresses. Their contents were therefore **not independently verified** here; no stronger external compatibility claim is made.
## Batch 1: Missing file lifecycle and presence safety (2026-10-13 historical decision; not a current acceptance result)

### Decision

- **Watcher does not automatically delete ledger rows**: filesystem deletion events mark matching active realtime ledger rows with `missing_at` instead of invoking GC on Media, Playback, Collections, or ledger rows. 45-second debounce expiration maintains memory state only and never deletes ledger rows.
- **Explicit administrator pruning**: missing ledger rows and file-derived rows (`file_meta`, fingerprint caches/attempts/stages, `scrape_sidecars` DB records) are only deleted upon explicit admin requests via `DELETE /api/v1/libraries/{id}/missing-rows`.
- **Strict ownership scoping**: presence verification, pruning, and all four scan types (full/incremental Movie/TV and Video) strictly enforce longest-prefix matching within the same MediaKind without falling back to kind defaults.
- **Root availability guard**: if any library root is unreadable or not a directory, presence verification and pruning fail closed with 409 `library.root_unavailable`. (Note: this does not detect disconnected NAS mounts where the mountpoint directory still exists empty).
- **User facts and media byte preservation**: Playback history/progress/favorites for all users, Media records, collection bindings, and disk bytes (NFO/sidecars/media) are preserved across missing file events and pruning.
