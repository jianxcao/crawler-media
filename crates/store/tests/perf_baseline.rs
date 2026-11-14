//! Storage performance baseline and deterministic regression gate.
//!
//! `"no performance regression"` is only a claim until something measures it. This binary
//! measures the hot read/write operations at a seeded, realistic scale and turns the two
//! metrics that actually predict trouble — **SQL statement count** and **full-table-scan
//! steps** — into hard assertions. Wall-clock numbers are reported separately;
//! this gate does not assert them, because a slow CI box would fail a healthy tree.
//! Measurement and comparison rules are in `docs/testing/storage-performance.md`.
//!
//! Run the realistic-scale gate in an exclusive build slot:
//!
//! ```bash
//! python3 scripts/with-build-lock.py --exclusive cargo test -j8 -p store --release --test perf_baseline -- --ignored --nocapture --test-threads=8
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
