//! R20/S02 measurement: public progress and linear input work on a 100k-Media
//! Library. Per-step timing is diagnostic, including global completion costs.
//! Ignored by default (seeding takes minutes); run with
//! `INTEGRITY_SCALE_N=100000 cargo test --release -p store --test library_integrity_scale -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::{Store, stats::StatsScope};

fn seed(store: &Store, n: usize) -> LibraryId {
    let lib = store
        .create_library(
            MediaKind::Movie,
            "Scale",
            &["/scale"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let library_id: LibraryId = lib.id.parse().unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id.as_str() == lib.id.as_str())
        .unwrap()
        .id;
    let mut files = Vec::with_capacity(100);
    for n in 0..n {
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: format!("Movie {n}"),
            year: Some(2024),
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: vec![],
        };
        store.insert_media(&media).unwrap();
        files.push(LibraryFile {
            file_id: LedgerId::new(),
            library_id,
            root_id: root_id.clone(),
            media_id: media.id,
            kind: MediaKind::Movie,
            path: format!("/scale/m{n}.mkv").into(),
            version: FileVersion {
                revision: 1,
                source_version: "v1".into(),
            },
            presence: FilePresence::Present,
            publication: FilePublication::Published,
            size_bytes: Some(1024),
            stat_known: true,
            created_ns: Some(0),
            modified_ns: Some(0),
            source_digest: None,
            source_path: None,
            transfer_mode: None,
            resolution: Some("1080p".into()),
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: None,
            tracks_version: 1,
            poster_path: None,
            backdrop_path: None,
            still_path: None,
        });
        if files.len() == 100 {
            publish(store, library_id, &files);
            files.clear();
            if (n + 1) % 5000 == 0 {
                eprintln!("seed progress: {} Media", n + 1);
            }
        }
    }
    publish(store, library_id, &files);
    library_id
}

fn publish(store: &Store, library_id: LibraryId, files: &[LibraryFile]) {
    if files.is_empty() {
        return;
    }
    let publications: Vec<StorePublication<'_>> = files
        .iter()
        .map(|file| StorePublication {
            file,
            units: &[UnitKey::WHOLE],
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        })
        .collect();
    let t = Instant::now();
    store
        .publish_files(
            &WriteScope {
                library_id,
                config_revision: 0,
            },
            &publications,
            1000,
        )
        .unwrap();
    if std::env::var_os("INTEGRITY_SCALE_TRACE").is_some() {
        eprintln!("publish {} files: {:?}", files.len(), t.elapsed());
    }
}

#[test]
#[ignore = "scale measurement; see module docs"]
fn integrity_checker_batches_stay_bounded_at_scale() {
    let n: usize = std::env::var("INTEGRITY_SCALE_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let seeded = Instant::now();
    let library = seed(&store, n);
    eprintln!("seeded {n} Media in {:?}", seeded.elapsed());

    store
        .begin_library_integrity(library, false, "op-scale", 1)
        .unwrap();
    let started = Instant::now();
    let mut steps = 0usize;
    let mut slowest = Duration::ZERO;
    let mut max_writer_ns = 0;
    let mut max_sql_rows = 0;
    let mut sql_rows = 0;
    let mut vm_steps = 0;
    let mut cursor = store
        .library_operation_view("op-scale")
        .unwrap()
        .unwrap()
        .cursor;
    loop {
        let batch = Instant::now();
        let (status, work) = store.with_stats(StatsScope::Library, || {
            store.advance_library_operation("op-scale", 2 + steps as i64, 100)
        });
        let status = status.unwrap();
        let took = batch.elapsed();
        steps += 1;
        slowest = slowest.max(took);
        max_writer_ns = max_writer_ns.max(work.writer_hold_ns);
        max_sql_rows = max_sql_rows.max(work.sql_rows);
        sql_rows += work.sql_rows;
        vm_steps += work.vm_steps;
        let progress = store.library_operation_view("op-scale").unwrap().unwrap();
        assert_ne!(
            progress.cursor, cursor,
            "each successful advance must progress"
        );
        cursor = progress.cursor;
        // This fixture has one file and one unit per Media. Allow phase transitions
        // and global completion without coupling the test to an exact phase count.
        assert!(
            steps <= n * 100 + 100,
            "step work must stay linear: {steps}"
        );
        if steps.is_multiple_of(10_000) {
            eprintln!("step {steps}: {took:?} (elapsed {:?})", started.elapsed());
        }
        if status.state != "applying" {
            assert_eq!(status.state, "succeeded");
            assert_eq!(progress.state, "succeeded");
            break;
        }
    }
    eprintln!(
        "checked {n} Media: {steps} steps, total {:?}, slowest step {slowest:?}, \
         max writer {max_writer_ns}ns, max SQL rows {max_sql_rows}, \
         total SQL rows {sql_rows}, VM steps {vm_steps}",
        started.elapsed()
    );
    assert!(sql_rows > 0 && vm_steps > 0, "capture actual input work");
    assert!(
        sql_rows <= n as u64 * 1_000 + 10_000,
        "SQL rows must stay linear: {sql_rows}"
    );
    assert!(
        vm_steps <= n as u64 * 500_000 + 100_000,
        "VM work must stay linear: {vm_steps}"
    );
    let report = store.library_integrity_report("op-scale").unwrap();
    assert!(!report.has_issues());
    assert_eq!(report.totals, Default::default());
    assert!(!report.truncated);
    let cursor = cursor.unwrap();
    assert_eq!(cursor["issues_found"], 0);
    assert_eq!(cursor["issues_repaired"], 0);
    assert_eq!(cursor["unrepaired"], 0);
    assert!(cursor["current"].is_null());
    assert!(
        store
            .active_library_operation(&library.to_string())
            .unwrap()
            .is_none()
    );
    store.ensure_library_query_ready(library).unwrap();
    // Timing is diagnostic: first/last steps can cover different phases, and FTS
    // preparation plus whole-Library finalization retain their documented costs.
}
