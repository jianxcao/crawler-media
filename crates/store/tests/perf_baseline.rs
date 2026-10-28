//! Task 17 of `docs/superpowers/plans/2026-10-26-storage-redesign-review-fixes.md`:
//! the performance baseline and the deterministic regression gate.
//!
//! `"no performance regression"` is only a claim until something measures it. This binary
//! measures the hot read/write operations at a seeded, realistic scale and turns the two
//! metrics that actually predict trouble — **SQL statement count** and **full-table-scan
//! steps** — into hard assertions. Wall-clock numbers are printed for humans and recorded in
//! `docs/audits/2026-10-26-perf-baseline.md`; they are never asserted, because a slow CI box
//! would then fail a healthy tree.
//!
//! Gate (run this after every plan task, compare the table with the audit doc):
//!
//! ```bash
//! cargo test -p store --release --test perf_baseline -- --ignored --nocapture
//! ```
//!
//! `cargo test -p store` also runs the cheap variant (`small_scale_gate`) on the same
//! fixture shape at 1/50 of the rows; every statement and full-scan limit is identical, so
//! a regression fails the default target too. Seeding 20k Media / 60k files through the
//! public store API costs minutes in a debug build, which is why the realistic run sits
//! behind `--ignored`/`--release`.

#[path = "perf_baseline/ddl_write_cost.rs"]
mod ddl_write_cost;
#[path = "perf_baseline/fixture.rs"]
mod fixture;
#[path = "perf_baseline/fixture_cache.rs"]
mod fixture_cache;
#[path = "perf_baseline/gate.rs"]
mod gate;
#[path = "perf_baseline/measure.rs"]
mod measure;
#[path = "perf_baseline/ops.rs"]
mod ops;
