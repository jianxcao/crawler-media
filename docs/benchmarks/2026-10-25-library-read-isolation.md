# Bounded Library read isolation — local evidence

Status: **B1–B5 are implemented and independently reviewed; final touched-crate/workspace gates passed on 2026-10-26.** The local release matrix and active-snapshot/cancellation proofs passed. Final Latest/logging/error-code refinements were not remeasured; the tracked source manifest and patch preserve the actual measured version. The [A9 report](2026-10-25-large-hit-query-read-model.md) and original [§9.1 SQL targets](../superpowers/specs/2026-10-25-large-hit-query-read-model.md#91-设计目标不是已测结果) remain the single-query workload evidence.

## Measurement contract

Base `394ae35`. Command: `cargo test --release -p api --test library_read_perf -- --ignored --nocapture`, wrapped in `/usr/bin/time -p`, log `/tmp/crawler-b5-release-2r1w.log`. The measured source is reconstructable from the tracked manifest and reverse patch linked below. Tracked source/log hashes, a reconstruction patch and the actual results are below.

Local Darwin 27.0.0 / arm64, 12 logical CPUs, 24 GiB RAM, APFS data volume; temp databases, no live instance, external network, NAS or real media device. Bundled SQLite version is printed by the harness. Process RSS uses macOS `ps -o rss` in KiB after fixture setup and at each shape's end. It is a process sample including fixture/setup and shared allocation, not request memory or peak RSS. OS caches are warm/uncontrolled; no OS cache eviction is claimed.

Each fresh shape has 5k or 100k Movie Media, one published File and one small zh-CN work NFO per Media, Drama on 50%, All on 100%, no adopted JSON, one admin User, no Playback marks, one populated Library plus the two empty defaults. It reuses the A9 fixture generator and public repair. Seed/repair precedes all request timing. It does not represent the independent 1M-file/Playback capacity shape or every §9.2 interaction.

Each round starts two real Router readers and one Store writer at an async barrier **before HTTP construction/authentication**, not after SQL snapshot establishment. Reader timing starts immediately after that barrier and runs through complete body collection; writer timing starts inside its blocking worker at the Store lock attempt and ends after commit/guard release, excluding dispatch queue time. The writer publishes one legal 100-File batch covering 100 distinct existing Media, changing only `size_bytes=round+1` with unchanged source version and exact totals; config/source checks, projection maintenance and commit are timed, fixture repair is excluded. This records overlapping requests and actual lock contention. Writer-first scheduling can put both readers behind its Store lock during authentication; the pool does not remove that wait. The separate deterministic WAL proof below establishes the pinned-snapshot writer boundary; the timing harness does not force writer commit while readers are paused.

- Page pair: Self `/api/v1/libraries/{id}/items?limit=100&include_total=true`, default Title ASC; Jellyfin `/Items?ParentId={id}&Recursive=true&IncludeItemTypes=Movie&SortBy=SortName&Limit=100`, Name ASC. Both exact totals are the fixture size.
- Genre pair: Self `/api/v1/libraries/{id}/facets?g=Drama`, all tiers with exclude-own semantics; Jellyfin same page shape with `Genres=Drama`. Both exact eligible totals are half the fixture size. All zero/static buckets are preserved.
- Each pair uses a separate fresh fixture, one first round, five warmups and fifty timed rounds. Readers are reported separately; writer distributions have fifty batch samples. Each round samples `wal_checkpoint(PASSIVE)` only after all readers/writer finish and after request timing.

The complete corpus first runs with SQL tracing **off** for HTTP latency. A separate fresh-fixture pass enables diagnostic SQL work. Diagnostic HTTP timings are not mixed into the trace-off distributions. The leased `sql_ms` is actual execution wall time including layout/transaction/cleanup, rather than SQLite's PROFILE sum; `sqlite_profile_ms` is separately labeled and has SQLite clock granularity. Diagnostic VM/fullscan/sort/statements cover initial reader layout, BEGIN, roster, selection/page inputs, ROLLBACK and final layout. They exclude pool checkout/connection-open validation and short Store/auth/binding/cold/final-validation SQL. `QueryWorkStats.rows_returned` and `hydrated_rows` are unfilled by this leased observer and their default zeros are not emitted or treated as measurements. Final response counts are measured separately; A9 retains its separately measured page-hydration evidence. Ordinary requests have absent diagnostic counters, rather than fabricated zero work. A9's sequential aggregate counters are not subtracted for concurrent requests.

## Request metrics and final response

A task-local handle is allocated independently of the absolute deadline and explicitly captured into each blocking worker. Both Self and Jellyfin timing middleware scope authentication through response construction. `elapsed_ms` uses `QueryTiming.started`; queue means spawn-to-worker-start; lock wait covers actual `try_lock_until`; held covers acquired guard lifetime including temporary SQLite budget policy. Device credential binding is separate from token lookup. Cold work and final validation retain the original deadline. Timeout records stage and sampled remaining budget, while a separate request-correlated late-worker record reports actual stop delay after the deadline. Logs contain no token, DeviceId binding value or raw SQL.

After review I1, workers register before dispatch. The HTTP completion record carries `pending_phases` and `metrics_final=false` while a worker remains queued or running; unfinished queue, SQL and Store wait/held duration fields are absent (`None`), rather than zero. A request-correlated worker-stop record carries the complete measured durations and SQL diagnostics after that worker exits; it is final when the response has completed and no workers remain. HTTP completion still returns promptly and lease/permit ownership lasts until worker exit. Auth/binding/cold stage totals retain their caller-side timing boundary; the actual worker queue and Store lock intervals are separate fields. Gated SQL, queued-auth and Store-lock tracing tests assert emitted events, including actual late SQL/queue/wait durations. This refinement was made after the measured release run below.

`returned`, exact available `total`, and retry count are taken from the final successful response after whole-attempt validation. Failure counts are absent. Latest has an array wire response; its multiple segments belong to one response and one retry boundary, and metrics do not publish discarded segment results. Latest records a total only when actually known: an explicit IsPlayed segment or both unplayed and played segments selected. When unplayed fills the page and played is unselected, total is absent. This final refinement adds no query for metrics.

## Active snapshots, writer and cancellation

The Router regression installs a diagnostic-only boundary after a completed `SELECT count(*) FROM libraries` inside each reader's transaction. Deferred BEGIN or the first VM callback alone is not treated as snapshot proof. Two readers pause at this established snapshot; a canonical 100-File/100-Media publication commits before either reader is released. A separate observer's PASSIVE checkpoint reports live frames versus frames checkpointed. WAL file length is sampled separately; PASSIVE busy=0 does not mean every frame was reclaimable.

Cancellation returns HTTP 503 while both reader leases and blocking permits remain occupied. Only after gates release do tests wait for worker rollback, progress/trace removal, lease and permit return, then run TRUNCATE successfully. No WAL/SHM unlink occurs. Controlled pauses test lifetime ownership; they do not emulate an uninterruptible slow disk or prove instant physical IO cancellation.

Causal RED/GREEN and focused verification supported the task and integrated reviews. Final full-gate totals are recorded below after actual successful completion.

## Actual local release results

Observed tool exit code **0** for the original command (exec session `29297`, completion chunk `6d4cc6`). The test ran 589.02 s; `/usr/bin/time` reported real 650.39 s including the release build. There was no original shell exit-code sidecar; this is the tool's observed status, not a status inferred from printed test text. The original log is preserved at `/tmp/crawler-b5-release-2r1w.log`, SHA-256 `124862cca5e7c8d8b8f7b9f1c7b5adb41aae8cd5fc3f5a16595d1f5efa0b648c`.

Eight fixtures × 56 rounds produced 896 raw reader rows and 448 raw writer rows. Batch0 is first, 1–5 warmup, 6–55 timed: 800 timed reader samples and 400 timed writer batches. Every response passed its exact total assertion. Rejections/timeouts were zero in all raw rows. Raw lines remain in the original log; [compact data](2026-10-25-library-read-isolation-data.txt) retains every fixture/first/summary, full first-round phase snapshots and diagnostic work vectors. Percentiles use nearest rank, readers separately, fifty timed samples per distribution. The diagnostic pass is not mixed with these tracing-off HTTP figures.

| Shape | Participant | First / p50 / p95 / max ms |
|---|---|---|
| page_5000 | Self items | 95.029 / 76.762 / 99.137 / 104.467 |
| page_5000 | Jellyfin Name | 164.981 / 152.645 / 186.137 / 195.225 |
| page_5000 | Writer, 100 Media | 91.472 / 73.557 / 95.584 / 101.182 |
| genre_5000 | Self facets | 97.214 / 77.721 / 80.437 / 85.967 |
| genre_5000 | Jellyfin Name | 161.161 / 146.798 / 154.047 / 157.012 |
| genre_5000 | Writer, 100 Media | 89.401 / 69.428 / 72.657 / 77.419 |
| page_100000 | Self items | 117.994 / 104.487 / 110.073 / 110.767 |
| page_100000 | Jellyfin Name | 191.551 / 175.915 / 187.628 / 191.603 |
| page_100000 | Writer, 100 Media | 114.645 / 101.229 / 106.255 / 107.476 |
| genre_100000 | Self facets | 213.640 / 206.434 / 220.602 / 236.012 |
| genre_100000 | Jellyfin Name | 201.306 / 198.137 / 213.613 / 229.122 |
| genre_100000 | Writer, 100 Media | 112.511 / 107.184 / 115.292 / 119.512 |

At 100k, the request-owned tracing-off phase samples show where time went. Durations below are p50 / p95 ms; auth includes its lock wait and stage totals overlap, so these columns should not be added as disjoint costs. Full wait/held/queue/binding/cold distributions are in the data artifact.

| Shape | Auth lock wait | Prepare held | Leased execution wall | Validation held |
|---|---|---|---|---|
| page_100000 R1 | 101.252 / 106.284 | 0.054 / 0.071 | 2.489 / 2.761 | 0.028 / 0.045 |
| page_100000 R2 | 101.215 / 106.243 | 0.210 / 0.251 | 7.385 / 7.833 | 1.641 / 1.799 |
| genre_100000 R1 | 107.204 / 115.323 | 0.055 / 0.078 | 99.034 / 105.490 | 0.029 / 0.041 |
| genre_100000 R2 | 107.175 / 115.283 | 0.180 / 0.228 | 10.802 / 12.195 | 2.079 / 2.310 |

The 100k page pair's ~101 ms median authentication wait coincides with a ~101 ms writer batch. The readers arrive before authentication, so the writer can acquire Store first. The pool improves the leased-query lock boundary; it cannot remove this short-stage competition. B has no uniform HTTP millisecond SLA. No new optimization is inferred from these numbers.

The separate diagnostic pass measured the following complete leased SQL work (timed medians; exact min/p50/p95/max including SQLite PROFILE and final counts are retained in the data). Complete scope includes real auxiliary queries: a page SELECT's zero sort count would not describe all leased work.

| Shape | VM steps | Fullscan steps | Sort operations | Statements |
|---|---|---|---|---|
| page_5000 R1 | 108294 | 24084 | 1 | 265 |
| page_5000 R2 | 129957 | 24286 | 101 | 765 |
| genre_5000 R1 | 383450 | 41578 | 0 | 261 |
| genre_5000 R2 | 137364 | 24286 | 101 | 765 |
| page_100000 R1 | 108294 | 24084 | 1 | 265 |
| page_100000 R2 | 129957 | 24286 | 101 | 765 |
| genre_100000 R1 | 5703450 | 374078 | 0 | 261 |
| genre_100000 R2 | 137364 | 24286 | 101 | 765 |

Physical WAL lengths include the post-seed file allocation. In particular 252,420,072 bytes was already present after 100k fixture repair; it is not 2R1W WAL growth. PASSIVE does not truncate that allocation. The checkpoint triple below is `(busy, log_frames, checkpointed_frames)` for the **last completed round**, sampled after reader exit, rather than a byte length or proof of physical truncation. The active-snapshot proof reports its distinct pinned result separately.

| Shape/pass | Post-seed WAL bytes | Maximum sampled WAL bytes | Last PASSIVE triple | RSS post-setup / end KiB |
|---|---|---|---|---|
| page_5000 HTTP | 13645472 | 13645472 | [0, 966, 966] | 43200 / 60256 |
| genre_5000 HTTP | 13645472 | 13645472 | [0, 961, 961] | 62256 / 48656 |
| page_100000 HTTP | 252420072 | 252420072 | [0, 920, 920] | 68608 / 75008 |
| genre_100000 HTTP | 252420072 | 252420072 | [0, 1929, 1929] | 76128 / 95712 |
| page_5000 diagnostic | 13645472 | 13645472 | [0, 976, 976] | 95936 / 76608 |
| genre_5000 diagnostic | 13645472 | 13645472 | [0, 929, 929] | 96320 / 96800 |
| page_100000 diagnostic | 252420072 | 252420072 | [0, 949, 949] | 69200 / 78480 |
| genre_100000 diagnostic | 252420072 | 252420072 | [0, 1955, 1955] | 69536 / 92704 |

## Reconstructing the measured source

The [tracked manifest](2026-10-25-library-read-isolation-source.json) records all 22 measured B5 Rust source hashes, the B4 base `394ae35`, original command and log hash. The tracked reconstruction artifacts remain sufficient after temporary task evidence is removed. Post-measurement changes comprise Latest known-total handling and its Router assertion, `execute_observed` documentation, pending/late-worker metrics logging with actual tracing tests, and the final integrated review's typed Self `library.reindexing` response mapping with Router regressions. Query selection and the harness are unchanged; final source has not been remeasured. Apply the [reconstruction patch](2026-10-25-library-read-isolation-final-to-measured.patch) **from final B5 source toward measured source** (`git apply --check`, then `git apply`) to reconstruct those exact measured files and remove the two post-measurement Rust files. Patch SHA-256: `31b0e353598a306e0b249dd94cb29c14f681606351dcaf020250f632da81a01a`. The manifest hashes verify all 22 reconstructed measured files plus three supplemental baseline files (`http/library/query.rs`, `library_read_principal.rs`, `library_read_concurrency.rs`) restored to their exact `394ae35` contents; these supplemental hashes are separate from the original measured hash entries. The tables and raw snapshots describe the original measured source; final Latest/logging/error-code refinements have not been timed.

## Final verification (2026-10-26)

Independent B5 and integrated A+B reviews passed after the consolidated typed rebuilding-error/documentation fix; no findings remain open. All four full commands then completed on one frozen source, with actual exit codes and log hashes retained in [verification metadata](2026-10-26-library-read-isolation-verification.json). The parent verified all 204 request-file hashes before documentation completion updates. No Rust/Web source changed afterward.

The first workspace execution session was lost after 358 completed result blocks (2649 passed, no recorded failure) without an exit code. Its log is retained as an interrupted attempt, not a passing gate. Only the full workspace command was repeated; the table records that completed retry. The verification metadata preserves both attempts and their log hashes.

| Command | Passed | Failed | Ignored | Exit |
|---|---:|---:|---:|---:|
| `cargo test -p store` | 607 | 0 | 7 | 0 |
| `cargo test -p api` | 1668 | 0 | 8 | 0 |
| `cargo test -p media-server` | 43 | 0 | 0 | 0 |
| `cargo test --workspace` | 2907 | 0 | 19 | 0 |

All 181 changed Rust files passed scoped rustfmt and hard file/function limits; local documentation destinations and whitespace checks passed. The dictionary has one definition and TOC entry for each of its 54 tables. Web is unchanged since A3's 848 tests/typecheck/build passed, so those gates were retained. Ignored performance programs are distinct from the explicit A9/B5 release runs above; field/capacity limits remain below.

## Limits

Not run: 100k Media / 1M Files / 1M Playback independent capacity, full Cartesian §9.2 interactions, NAS/real media devices, power loss, OS cold cache and uninterruptible device IO. These local measurements do not establish a universal HTTP SLA, eliminate short Store contention, or show that adding a read pool makes one SQL statement faster. The publication shape is 100 Files across **100 Media**; A9 also measured a 100-File/**one Media** publication shape. Those are different projection workloads, so no publication speedup ratio is claimed. A9 HTTP was a single request and used the actual default Jellyfin order; this corpus explicitly requests Name. No paired HTTP speedup ratio is claimed. No Web source changed; no new Web gate is needed.
