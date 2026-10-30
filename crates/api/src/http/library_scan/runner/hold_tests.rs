//! How long does the scan hold the Store lock, and does it grow with the Library?
//!
//! A reader (heartbeat, playback progress, browse) queues behind one hold, so the number that
//! matters is the longest single section, not the scan's total time. This drives the real
//! `ScanContext::walk_and_process` synchronously on this thread (no coordinator, no polling) so
//! the sections it records belong to exactly one scan.
//!
//! The fixture is one series: every episode file resolves to the same Media, which is the shape
//! that makes per-batch summary work visible. `SCAN_HOLD_EPISODES` scales it for the 25 / 100 /
//! 400-file batch comparison.

use std::sync::Arc;

use domain::MediaKind;

use crate::management::ApiState;

struct NoFetch;

impl indexer::Fetcher for NoFetch {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("offline".into()))
    }
}

/// TV Library with one root; the caller writes the episode files.
fn tv_fixture() -> (tempfile::TempDir, ApiState, String, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path().join("data")).unwrap();
    let state = ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap();
    let root = dir.path().join("series");
    std::fs::create_dir_all(root.join("Season 01")).unwrap();
    let library_id = {
        let store = state.store();
        let guard = &store;
        guard
            .create_library(
                MediaKind::Tv,
                "Series",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
    };
    (dir, state, library_id, root)
}

fn write_episodes(root: &std::path::Path, count: usize) {
    let season = root.join("Season 01");
    for index in 0..count {
        let path = season.join(format!("Series S01E{:04}.mkv", index + 1));
        std::fs::write(path, b"episode").unwrap();
    }
}

fn episodes_from_env(default: usize) -> usize {
    std::env::var("SCAN_HOLD_EPISODES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[derive(Debug, Default)]
struct SectionStats {
    section: &'static str,
    count: usize,
    p50: u64,
    p95: u64,
    p99: u64,
    max: u64,
}

fn percentile(sorted: &[u64], percent: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) * percent) / 100]
}

fn summarise(samples: &[super::hold::ScanHoldSample]) -> Vec<SectionStats> {
    let mut sections: Vec<&'static str> = samples.iter().map(|sample| sample.section).collect();
    sections.sort_unstable();
    sections.dedup();
    sections
        .into_iter()
        .map(|section| {
            let mut micros: Vec<u64> = samples
                .iter()
                .filter(|sample| sample.section == section)
                .map(|sample| sample.micros)
                .collect();
            micros.sort_unstable();
            SectionStats {
                section,
                count: micros.len(),
                p50: percentile(&micros, 50),
                p95: percentile(&micros, 95),
                p99: percentile(&micros, 99),
                max: micros.last().copied().unwrap_or(0),
            }
        })
        .collect()
}

/// Runs one full scan over the fixture and returns the recorded lock sections.
fn scan_once(
    state: &ApiState,
    library_id: &str,
    root: &std::path::Path,
) -> Vec<super::hold::ScanHoldSample> {
    // A real operation row: the chunk loop then exercises the checkpoint and scan_seen paths.
    let op_id = {
        let store = state.store();
        let guard = &store;
        guard
            .enqueue_scan(library_id, "hold-fixture", &[], 1_000)
            .unwrap()
            .id
    };
    let lease = state.acquire_library_work(library_id).unwrap();
    let scan_scope =
        crate::http::library_scan::scope::ScanScope::capture(state, library_id).unwrap();
    let ctx = super::walk::ScanContext {
        state,
        id: library_id,
        lease: &lease,
        scan_scope: &scan_scope,
        kind: MediaKind::Tv,
        scrape: false,
        op_id: Some(&op_id),
    };
    super::hold::start_scan_hold_trace();
    let started = std::time::Instant::now();
    let (report, totals) = match ctx.walk_and_process(root, root) {
        Ok(result) => result,
        Err(failure) => panic!("scan must succeed: {}", failure.error),
    };
    let elapsed = started.elapsed();
    let samples = super::hold::take_scan_hold_trace();
    println!(
        "EPISODES={} visited={} inserted={} scan_ms={} sections={}",
        episodes_from_env(0),
        report.visited,
        totals.inserted,
        elapsed.as_millis(),
        samples.len()
    );
    for (index, sample) in samples.iter().enumerate() {
        println!("  #{index:<3} {:<20} {}us", sample.section, sample.micros);
    }
    for stats in summarise(&samples) {
        println!(
            "  {:<20} n={:<4} p50={:>7}us p95={:>7}us p99={:>7}us max={:>7}us",
            stats.section, stats.count, stats.p50, stats.p95, stats.p99, stats.max
        );
    }
    samples
}

/// Measurement: per-section lock-hold distribution for one 100-file-per-chunk scan.
///
/// Printed, not asserted: the held-duration numbers are wall clock on whatever machine runs the
/// suite. The regression pin lives in `scan_lock_sections_are_per_chunk_and_bounded`.
///
/// `SCAN_HOLD_EPISODES=400` (or more) turns this into the scaling run: the plan section's cost
/// grows with the Media's already-published files, so the per-chunk number is the interesting
/// one, not the scan's total.
#[test]
fn measure_scan_lock_sections() {
    let episodes = episodes_from_env(100);
    let (_dir, state, library_id, root) = tv_fixture();
    write_episodes(&root, episodes);
    let samples = scan_once(&state, &library_id, &root);
    assert!(
        !samples.is_empty(),
        "the trace must record the scan's sections"
    );
    let total: u64 = samples.iter().map(|sample| sample.micros).sum();
    let longest = samples
        .iter()
        .map(|sample| sample.micros)
        .max()
        .unwrap_or(0);
    println!(
        "TOTAL_HOLD_MS={} LONGEST_HOLD_MS={}",
        total / 1_000,
        longest / 1_000
    );
}

/// Structural pin: the Store lock is taken a fixed number of times per chunk, never per file.
///
/// Deterministic — it counts sections, not milliseconds. If a future change moves the lock
/// inside the per-file loop, the count stops scaling with chunks and starts scaling with files,
/// which is exactly the regression that makes every bounded reader queue.
#[test]
fn scan_lock_sections_are_per_chunk_and_bounded() {
    const PER_CHUNK: usize = 100;
    let episodes = 2 * PER_CHUNK;
    let (_dir, state, library_id, root) = tv_fixture();
    write_episodes(&root, episodes);
    let samples = scan_once(&state, &library_id, &root);

    let chunks = episodes.div_ceil(PER_CHUNK);
    // Per chunk: checkpoints, plan, record, seen — plus the op-level and invalidate paths when
    // they run. `enqueue_probes` and `scan_media_for` are recorded per chunk as well.
    let per_chunk_cap = 8;
    assert!(
        samples.len() <= chunks * per_chunk_cap + per_chunk_cap,
        "{} lock sections for {chunks} chunks ({episodes} files) is more than {per_chunk_cap} per chunk: {:?}",
        samples.len(),
        summarise(&samples)
    );
    let recorded: Vec<&str> = samples.iter().map(|sample| sample.section).collect();
    for section in ["checkpoints", "plan", "record"] {
        assert!(
            recorded.contains(&section),
            "{section} must be measured, got {recorded:?}"
        );
    }
}
