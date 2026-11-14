# ADR-0012: Library removal preserves files and purges orphan Media

- Status: Accepted.
- Scope: Library deletion, orphan Media cleanup, file presence and NFO reimport.
- Storage: file facts are in `library_files`, probe details in `file_details`, and Media/Playback share `library.db`. Other database responsibilities are defined in [ADR-0008](0008-sqlite-system-of-record.md); there is no cross-file atomic transaction.

## Context

A Library configuration, its file facts, a canonical Media, and a User's Playback history have different lifetimes. Removing a configured root preserves disk content. Media left without any file or retaining business reference are purged with their Playback facts. Recreating a root creates a new Library and new file rows; matching only a filename or reusing a path is not sufficient to recover identity safely.

Store uses four independent SQLite connections/files, plus a separate Jobs queue connection/file. An ordinary transaction on one connection does not atomically commit the others. Cancellation notification alone also does not prove an old worker has stopped publishing: blocking scans and child processes can outlive the requesting future.

## Decision

### Remove Library facts and purge unreferenced Media

Removal deletes the strict ownership snapshot's file facts, derived metadata, fingerprint caches/samples/attempts and probe-stage state. When a Media loses its last Library file and is not kept by a collection entry or a subscription, it is purged together with every row it owns (aliases, metadata, episodes, markers, probe/fingerprint state and each User's Playback position, watched state and favorite state) — the same rule as single-file delete (`library_write/orphan_media.rs`). Media still backed by another Library's file or kept by a collection/subscription keep their Playback facts. Historic logs (`playback_logs`, `playback_metrics`, removal receipts), Subscribe, Downloader configuration and Catalog caches are retained. Video, STRM, subtitles, NFO, poster, fanart and episode artwork are neither removed, moved nor rewritten by this operation.

Ownership is captured before roots disappear, using the longest uniquely matching root and directory-component boundaries. Default-library fallback is not deletion ownership. Ambiguous roots fail preflight rather than guessing. Nested independent libraries and another Library's file for the same Media remain intact. Shared Media/episode facts cannot be cleared while another file still needs them.

The last Library of a kind remains protected under the existing default-Library/Transfer contract. Removing a non-last default promotes an existing Library only at finalization. Supporting zero libraries of a kind is a separate change.

### Durable operation and publication barriers

Admin `DELETE /api/v1/libraries/{id}` returns `202` with an operation identifier, including when cleanup is fast. Admin `GET /api/v1/library-removals/{operation_id}` exposes `frozen`, `draining`, `cleaning`, `succeeded` or `failed`, the snapshot removal count and any error. Repeated requests use the existing operation.

The sequence is freeze/snapshot → prevent new work → cancel all queued/running work of the Library → delete immediately → idempotent scoped cleanup → delete root configuration → succeeded. Removal does **not** wait for in-flight scan/probe/marker/scrape work or leases to drain: cancellation returns only after every publication gate is closed, so a late publication (covers included) is refused, and the Store refuses any write into a Library under removal (`library_removals` fence; FKs once the row is gone). Canceled work notices the cancellation on its own and its result is discarded (`crates/api/src/library_lifecycle/removal.rs`).

The durable operation stores the cleanup scope so a restart or partial cross-database cleanup can resume safely. It does not pretend the four databases share a transaction. Cleanup retries must preserve the snapshot count and not revive canceled work. Frozen libraries are hidden from ordinary browsing/new Playback and remain visible for administrative status/retry. Static artwork read endpoints remain public under the existing image-authentication contract.

After Library finalization commits, explicit Subscribe bindings to that Library are cleared in a separate IMMEDIATE transaction on subscribe.db. The Subscribe itself is retained and subsequent Transfer resolves the current default Library of the same MediaKind. If that transaction fails or the process stops between commits, the durable succeeded removal receipt drives idempotent detachment on retry or startup; cleanup never acquires write locks on both databases together.

Subscribe retention in library.db is the authority for orphan Media cleanup. Creation records a generation and epoch in subscribe.db, retains the reference in library.db, acknowledges it, then commits the Subscribe; interrupted stages recover from durable state and a closed generation cannot reopen. Deletion records the exact generation release and Jobs cleanup intent with the business deletion. Compensation protects preexisting Media facts when creation fails. Collection deletion and Media merge use durable fences and bounded windows; merge copies retention, repoints Subscribes and releases old references before finishing. These are separate commits, not a cross-database transaction. Closed generation fences, terminal epochs and tombstones may accumulate; bounded batches do not imply bounded total storage or a compaction guarantee.

The durable succeeded receipt records Library finalization. Public removal status and repeated DELETE responses continue to report `cleaning` while Subscribe bindings to that Library remain, and expose `succeeded` only after the independent detachment commit is visible. This uses the existing states and does not rewrite the durable receipt during recovery.

### Reimport identity comes from reliable work-level aliases

Reimport creates new file identifiers and reuses an existing MediaId only when the work identity is reliable. External aliases include MediaKind: equal numeric TMDB movie and TV identifiers do not merge works. Conflicting provider aliases must fail rather than choose an arbitrary Media.

Movie identity comes from the matching movie NFO. TV identity comes from bounded ancestor `tvshow.nfo` lookup inside the Library root. An `episodedetails` NFO can supply season/episode facts, but its episode title/identifier is not a TV Media identity. Valid NFO identity and display metadata take precedence over a confident filename. Title-only fallback is allowed only when unique and non-conflicting; absent or ambiguous identifiers do not guarantee history recovery. Old history is retained even when it cannot safely be associated with a newly scanned file.

A complete local identity plus display title avoids implicit Catalog search/details and existing-sidecar rewrites; an explicit refresh may still fetch fresh metadata. A later online identification must converge the new file on the canonical MediaId before publication rather than attach the same alias to a competing Media.

NFO metadata is not stream-probe output. Reimport schedules fresh media probing for new file facts; NFO runtime/streamdetails do not establish probe completion. New libraries keep `detect_intros=false` and `enable_fingerprint=false`; these switches do not disable basic media probing.

## Consequences and acceptance boundaries

- Reliable MediaId reuse reconnects Playback facts only for Media that survived removal (kept by another file, collection or subscription); purged orphan Media come back as new Media without old favorites/progress.
- Files stay available to external tools throughout removal. The application must stop offering new Playback through removed file rows.
- Removal no longer blocks on slow work; a removal that fails in cleanup may be retried and stays frozen until it succeeds.
- Reimport requires explicit ownership and reliable identity; a matching path alone does not authorize attaching another Media's history.

The public API regression source in `crates/api/tests/library_delete_reimport.rs` covers per-episode and whole-movie positions, favorite/watched states, fresh file identity, derived-detail/fingerprint/stage cleanup, preserved sidecar bytes, another Library's same-Media file/cache, default-off detection switches, and conflicting aliases. It constructs ApiState before entering Tokio and injects an offline Catalog; this checks cache non-reuse but does not prove fresh probe execution, real streaming or child-process cancellation. Creation schedules an automatic scan; the explicit public scan is idempotent with that path. History recovery only holds where the Media is retained by another file, collection or subscription.

Restart-mid-clean, canceled-job recovery, publication races, ambiguous no-ID reimport, nested-root ownership, member visibility, Subscribe-target handling and fake-probe completion require focused suites and UI regression cases. Record results separately using the [system regression cases](../testing/system-regression-cases.md); a design document is not an execution receipt.

## Missing file lifecycle and presence safety

### Decision

- **Watcher does not automatically delete file rows**: filesystem deletion events mark matching active realtime file rows with `missing_at` instead of invoking GC on Media, Playback, Collections, or file rows. 45-second debounce expiration maintains memory state only and never deletes file rows.
- **Presence modes**: verification marks missing files; `DELETE /api/v1/libraries/{id}/missing-rows` prunes rows already marked missing after rechecking the source. A full rescan can purge files proven absent. `LedgerPresenceMode` and identity checks in `crates/store/src/ledger_presence.rs` define these separate operations.
- **Strict ownership scoping**: presence verification, pruning, and all four scan types (full/incremental Movie/TV and Video) strictly enforce longest-prefix matching within the same MediaKind without falling back to kind defaults.
- **Root availability guard**: if any library root is unreadable or not a directory, presence verification and pruning fail closed with 409 `library.root_unavailable`. (Note: this does not detect disconnected NAS mounts where the mountpoint directory still exists empty).
- **Preservation and orphan cleanup**: a missing event alone preserves Media and User facts. Pruning removes file-derived facts and applies the orphan rule above; retained Media keep User facts. These presence operations do not delete disk bytes.
