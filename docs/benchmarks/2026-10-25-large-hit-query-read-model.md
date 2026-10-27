# Large-hit query read model: release verification

Status: **A Store read-model targets pass on the measured local 100k shape.** B read isolation has passed independent task/integrated reviews and final touched-crate/workspace gates; see [local concurrency/WAL/request-metric and verification evidence](2026-10-25-library-read-isolation.md). Independent 1M-file/Playback capacity and field conditions remain Not run. The [compact measurement data](2026-10-25-large-hit-query-read-model-data.txt) preserves each measured row; the [spec](../superpowers/specs/2026-10-25-large-hit-query-read-model.md#91-设计目标不是已测结果) retains the original budgets.

## Method and provenance

- Baseline production code is pinned to `1085a9ac0be0843640a1d6d2d01cdce578a918be` in disposable `/tmp/a9-baseline-1085a9a`. Parent independently byte-compared all 180 archived Store source and `Cargo.lock` members with that commit: zero mismatches. Only fixture adapters changed in that tree; its production SQL/schema remained intact. Current source is `59e21620a15704e00ff4babf49a4adbae6a7f336` plus A9 changes. Each version built a fresh temporary database. Current derived rows came from public Integrity repair; no existing instance data was used.
- The exact two-file [baseline fixture patch](2026-10-25-large-hit-query-read-model-baseline-fixture.patch) is SHA-256 `d19a656a5c8e56587a87c43646a32de9d010d6210fe9b8e41bc23660ee2ad098`. It contains only the baseline test adapter and harness, with no production paths. `git apply --check` and application against a fresh archive of the pinned commit reproduced both baseline adapter files byte-for-byte. For a rerun, archive the pinned commit into a disposable directory, apply that patch there, then run `PERF_SIZES=5000,20000,50000,100000 cargo test --release -p store --test query_model_perf -- --ignored --nocapture --test-threads=1` sequentially in that directory.
- Same Apple M4 Pro, 24 GiB RAM, Darwin 27.0.0 arm64, rustc/Cargo 1.93.1 release `aarch64-apple-darwin`, Cargo.lock SHA-256 `bb27ea0bfd3ef6385191f09b9cb777d11f2b40fdddd0de51f64775c4791a74d2`, and bundled SQLite 3.50.2. Timed runs were sequential, without another heavy test during samples. No SQLite PRAGMA tuning or external query service was added.
- Shared deterministic v5 Library/root/Media/File/User IDs, insertion order, year 2024, one present/published File and whole-Movie coverage per Media, and fixed revisions/clocks/locale. The paired raw fixture has valid zh-CN work NFO genres alternating Drama/Comedy plus All, with **no adopted Metadata**. This is NFO/no-adopted-Metadata, not a truly metadata-free fixture. The old adapter inserts coherent old derived rows; current Integrity repair derives the new projection. Independent visible-ID/year/stock/total oracles passed in both trees (`/tmp/a9-baseline-oracle.log`, `/tmp/a9-current-oracle.log`).
- Every query has one separately reported first call, five warmups, and 50 timed calls. p50/p95 use nearest rank at sorted indices 24/47. Store duration surrounds the public instrumented method. SQLite STMT/PROFILE/ROW events provide actual VM, fullscan, sort, statement, SQL row, and SQL duration counts. Ordinary Store queries do not install this observer. Native SQLite PROFILE can report 0 ns for short SQL; the Store wall duration is separate. `rows_returned` is public page/bucket rows, while `total` is the exact logical hit count.

## §9.1 target verdict at 100k

| Query, limit 100 | Target p50 / p95 ms | Baseline p50 / p95 | Current p50 / p95 | Verdict |
|---|---:|---:|---:|---|
| Unfiltered facets | 20 / 50 | 286.161 / 313.446 | 0.120 / 0.123 | Pass |
| Drama 50% facets | 80 / 150 | 343.202 / 380.873 | 71.734 / 76.413 | Pass |
| All 100% facets | 80 / 150 | 365.701 / 395.440 | 0.121 / 0.131 | Pass |
| Drama + decade + stock facets | 80 / 150 | 357.605 / 423.066 | 72.644 / 76.957 | Pass |
| Browse default Title ASC page | 20 / 50 | 6.585 / 9.001 | 0.111 / 0.119 | Pass |
| Protocol Name ASC page | 20 / 50 | 245.938 / 264.911 | 0.280 / 0.283 | Pass |
| Browse Drama 50% page, exact total | 30 / 75 | 52.892 / 60.096 | 0.237 / 0.245 | Pass |

The real Jellyfin no-`SortBy` Work request is **explicit** `[DateCreated DESC, Name DESC]`, distinct from Browse Title ASC and synthetic protocol Name ASC. Its current 100k Store row was 0.296/0.300 ms in the orthogonal metadata fixture, and its real HTTP route is measured below. Multi-filter `items_multi` returned exact total 50k at 85.653/94.664 ms; §9.1 has no uniform millisecond cap for arbitrary combinations.

The unfiltered facets VM count was 564 at every 5k/20k/50k/100k size with fixed buckets. Drama facets at 100k used 2,100,747 VM and 199,997 fullscan steps for exact eligible-member work. The 400→4000 public regression rejects quadratic filtered work. The separate cold-table authorizer refusal regression proves the no-cold-table path; a small returned row count alone would not.

## Paired curves, work, and footprint

The [compact data](2026-10-25-large-hit-query-read-model-data.txt) includes all 13 paired queries at 5k/20k/50k/100k, with first/p50/p95/max, logical total, returned and SQL rows, VM, fullscan, sort, statements, SQL duration, and observed hydration. `kind_library` and `kind_root` use a second fixture with fixed 1000 Movie Media and TV growing to 5k, 20k, 50k, and 100k; the query selects Movie. The final accepted current log is `/tmp/a9-current-final-v2-release.log`; `/tmp/a9-current-final-release.log` predates the actual Jellyfin default-order index and is preserved as a pre-integration iteration. The pinned baseline log is `/tmp/a9-baseline-release.log`.

At 100k the paired baseline fixture setup was 55,272.876 ms, with DB/WAL files 670,752,768 / 674,942,552 bytes while open. Current raw insertion plus public repair was 126,779.798 ms, with DB/WAL 874,631,168 / 252,374,752 bytes. The current DB file is 30.4% larger, reflecting the query projection and complete-order index. Version-specific fixture adapters and default checkpoint behavior differ; setup is excluded from query latency and these numbers are not ordinary publication throughput or a controlled checkpoint comparison. The data artifact retains the 5k/20k/50k footprint rows too.

## Orthogonal §9.2 representatives (current version only)

These are **not paired baseline speedup ratios**. Each listed query used the same first + five warmups + 50 timed convention, with raw-row and public logical-total oracles. The data artifact includes every measured row and its complete work vector. The release logs are `/tmp/a9-expanded-100k-release.log` and `/tmp/a9-axes-final-release.log`.

| Data/request representative | Size and exact total | p50 / p95 ms |
|---|---:|---:|
| True no-NFO/no-adopted-Metadata, unfiltered Work / facets | 100k / 100k | 0.116 / 0.117; 0.127 / 0.129 |
| NFO/no-adopted-Metadata, Name DESC; offset 50k; limit 0; empty offset 100k | 100k / 100k | 0.288 / 0.299; 1.887 / 1.921; 0.041 / 0.042; 3.485 / 3.673 |
| Three-locale NFO/adopted conflict, >8KiB JSON, zh-CN Genre Rare 1%, Work / facets | 100k / 1k | 4.511 / 4.670; 43.864 / 52.651 |
| Same metadata fixture, protocol empty sort / explicit API default / Web cursor after 50k | 100k / 100k | 0.293 / 0.299; 0.296 / 0.300; 0.125 / 0.132 |
| Actor 0 / 20 / 100 per Media, unfiltered protocol | 1k / 1k | 0.256 / 0.263; 0.256 / 0.259; 0.260 / 0.271 |
| Actor 20 / 100, scoped person filter | 1k / 1k | 2.040 / 2.089; 2.462 / 2.508 |
| Same Media overlapping 2 / 5 Libraries, Root logical dedup | 1k / 1k | 2.732 / 2.812; 5.777 / 5.902 |
| Empty / sparse 1% / dense 100% User favorite+played | 1k / 0, 10, 1k | 0.847 / 0.858; 0.936 / 0.952; 1.439 / 1.466 |
| Sparse 1% / dense 100% User Watch+Drama facets | 1k / 5, 500 | 1.627 / 1.654; 3.988 / 4.019 |
| TV Series / Season / Episode explicit Unit order / Folder Episodes / Mixed | 1k / 1k, 1, 1, 1k, 2k | 0.254 / 0.259; 0.258 / 0.266; 0.360 / 0.367; 2.426 / 2.450; 2.415 / 2.455 |
| Unicode original-title ties, DateCreated DESC + Name DESC | 100 / 100 | 0.252 / 0.255 |

The high-end metadata fixture had 300k adopted rows with `details_json` longer than 8KiB, 300k work NFO rows across zh-CN/en-US/ja, and 1000 zh-CN Rare facts. Its open DB/WAL lengths were 3,630,919,680 / 2,840,781,232 bytes. The measured projection selected zh-CN; NFO Rare won over the conflicting adopted genre. Timed activation of en-US or ja as selected default was **Not run**. Actor projections were independently counted at 0/20k/100k people for 1k Media. Root overlap pages returned 1k unique IDs with both two and five Libraries. The 1k User shapes had exact 0/10/1000 favorite+played totals and 0/5/500 Watch+Drama totals. The 1k TV shape had exact 1000 Series, 1000 Folder Episodes, and 2000 Mixed hits. The 100-Movie Unicode test asserts full visible ID order and original-title ties, beyond totals.

## Real HTTP phases and publication

The 100k release HTTP harness used fake Fetcher and Downloader and the real self and Jellyfin routers. It timed Router request start through full response-body collection. Phase counters sample actual boundaries in the same request; no subtraction of a separately sampled Store call is called serialization. Selection includes the overlapping Store select duration. Lock wait is separate. Selected facts includes page inputs, Series scalar when present, Unit index, and selected-light construction; cold details times heavy snapshots. This Movie request had no Series scalar. It actually materialized 300 selected File/coverage/Media/User fact rows per request.

| 100k route or phase | Output | First / p50 / p95 / max ms |
|---|---:|---:|
| Self items | 45,420 bytes | 1.603 / 0.605 / 0.614 / 0.625 |
| Self facets | 1,273 bytes | 0.502 / 0.222 / 0.260 / 0.305 |
| Jellyfin Items | 70,387 bytes; 300 hydrated rows | 67.847 / 72.193 / 79.667 / 85.343 |
| Jellyfin lock wait | — | 0.000 / 0.000 / 0.000 / 0.339 |
| Jellyfin selection | — | 1.017 / 0.676 / 0.810 / 0.815 |
| Jellyfin Store select (inside selection) | — | 0.922 / 0.634 / 0.763 / 0.771 |
| Jellyfin selected facts | — | 5.249 / 4.491 / 4.781 / 6.226 |
| Jellyfin cold details | — | 60.454 / 66.355 / 73.310 / 77.445 |
| Jellyfin DTO | — | 0.420 / 0.340 / 0.359 / 0.368 |
| Jellyfin actual serialization | — | 0.136 / 0.133 / 0.139 / 0.141 |

The printed 0.000 ms median lock wait is rounded to three decimals; the focused test observed positive nanoseconds. Before the exact explicit default-order index, the same Jellyfin route was 160.079/167.071 ms p50/p95 with Store select 86.059/89.832 ms. The final route is 72.193/79.667 ms with Store select 0.634/0.763 ms. §9.1 specifies Store targets, not a uniform HTTP millisecond limit. The remaining cold-detail time is reported without calling it a Store target miss. Final HTTP log: `/tmp/a9-http-final-v2-release.log`.

Final-schema release publication preserved the 100-file/one-Media and 100-actor insert/delete/unchanged member oracles (`/tmp/a9-publication-final-release.log`). Each case is **one** measured publish, not a p50/p95 throughput curve.

| 100-file publication | Publish ms | Summary writes | Open DB / WAL bytes |
|---|---:|---:|---:|
| No User state | 24.371 | 0 | 4,096 / 1,512,072 |
| 40 Users with state | 25.265 | 40 (one per User per Media) | 4,096 / 2,892,272 |
| 100 actors, protocol membership trigger oracle | 32.516 | 0 | 4,096 / 1,668,632 |

## Acceptance limits and remaining gates

- **Not run:** independent 100k Media / 1M Files / 1M Playback capacity; high-end 100k overlap, dense User, or TV hierarchy; the full Cartesian product of §9.2 values. The explicitly sized representatives above exercise each local axis without implying larger interactions were covered. Fixed 1000 Movie with TV 5k→100k **was** measured in the paired curve.
- **Not run in A9:** timed en-US/ja activation of the three-locale default; B two-reader/one-100-file-writer contention and read-pool metrics; NAS/real device, physical power loss, and OS cold-cache. The later [B5 report](2026-10-25-library-read-isolation.md) records the completed local contention/pool measurements. Fresh temporary databases and first-call timings do not substitute for an OS cold-cache run; the other field limits remain unrun.
- Same-version repair, rollback/recovery, frozen-writer, member/counter, and cold-table authorizer regressions are exercised by Store/API tests. Full touched-crate gates passed on the frozen source: Store **577 passed / 0 failed / 7 ignored**, API **1615 / 0 / 7**, and media-server **38 / 0 / 0**. The 25 changed Rust files passed explicit rustfmt; changed-file/function hard limits, whitespace, local documentation links, and the DDL dictionary checks passed. No Web source changed after A3, whose 848-test/typecheck/build gates remain the last relevant Web verification. The later shared `cargo test --workspace` and final touched-crate gates passed after A+B integrated review; [B's final verification](2026-10-25-library-read-isolation.md#final-verification-2026-10-26) records their counts. This paragraph preserves the historical A9-stage results and does not claim field capacity acceptance.

## Failed iterations preserved

The first current release run stopped after a 5k filtered-facets failure: Drama p50/p95 about 310/329 ms, 50.6M VM, and 12.5M fullscan steps (`/tmp/a9-current-release.log`). A public 400→4000 regression found 88× VM growth for 10× facts (`/tmp/a9-filtered-red.log`). Exact set intersection, provable universe/empty bucket identities, and grouped partial intersections reduced work without changing exclude-own semantics. A later four-size iteration still failed 100k Drama/three-dimensional facets at 182.73/198.26 and 190.36/203.94 ms (`/tmp/a9-current-final-marginal-release.log`); it is not the accepted curve. Separately, the real Jellyfin default order initially performed a full single-Library window scan: public 400→4000 work was 51,680→441,246 VM before and 10,153→10,154 after the complete-order index and single-Library dedup rule (`/tmp/a9-http-default-red.log`, `/tmp/a9-http-default-green.log`).
