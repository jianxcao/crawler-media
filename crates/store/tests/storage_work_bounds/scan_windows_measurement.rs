use std::path::Path;
use std::time::{Duration, Instant};

use store::ScannedLedger;

use super::scan_windows::{
    MediaRow, count_in, fixture_media, insert_medias, media_rows, scanned_row,
};
use super::*;

fn library_bytes(data_dir: &Path) -> (u64, u64) {
    let size = |name: &str| {
        std::fs::metadata(data_dir.join(name))
            .map(|meta| meta.len())
            .unwrap_or(0)
    };
    (size("library.db"), size("library.db-wal"))
}

fn rss_kb() -> Option<u64> {
    let pid = std::process::id().to_string();
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

fn timed_window(store: &Store, rows: &[ScannedLedger], now_ms: i64) -> f64 {
    let start = Instant::now();
    store
        .insert_scanned_files(rows, now_ms)
        .expect("window commits");
    start.elapsed().as_secs_f64() * 1000.0
}

/// Holds the `library.db` write lock for `hold`, then releases it.
fn hold_write_lock(db: &Path, hold: Duration) -> std::thread::JoinHandle<()> {
    let holder = rusqlite::Connection::open(db).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(hold);
        holder.execute_batch("ROLLBACK").unwrap();
    })
}

/// Step 2 diagnostic for the store side: prints per-window transaction durations,
/// throughput and storage growth for a mixed scan workload (20 windows, 2,000 files).
/// The printed numbers are measurements of this machine and this run, not promises;
/// only the committed facts are asserted.
#[test]
fn scan_window_cost_measurement() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();
    let lib = store
        .create_library(
            MediaKind::Movie,
            "Measure",
            &["/measure"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let medias = insert_medias(&store, 100, MediaKind::Movie);
    let bulk_media = medias[0];

    let (db_before, wal_before) = library_bytes(&data_dir);
    let mut window_ms: Vec<f64> = Vec::new();
    let mut files = 0usize;

    // 10 windows: one new file per Media (incremental reduce per affected Media).
    for round in 1..=10u32 {
        let rows: Vec<ScannedLedger> = medias
            .iter()
            .map(|media| scanned_row(*media, &format!("/measure/m{media}/r{round:02}.mkv"), None))
            .collect();
        files += rows.len();
        window_ms.push(timed_window(&store, &rows, 1_000 + round as i64));
        // One committed window reduces the bulk Media exactly once, so the reduction
        // counter equals the number of windows reduced so far.
        let after = media_rows(&data_dir, &lib.id);
        assert_eq!(
            after[&bulk_media.to_string()].item_revision,
            round as i64,
            "incremental window {round} must add exactly one reduction"
        );
    }
    // 10 windows: 100 new files of a single Media (one reduce for the whole window).
    for round in 1..=10u32 {
        let rows: Vec<ScannedLedger> = (0..100)
            .map(|index| {
                scanned_row(
                    bulk_media,
                    &format!("/measure/bulk/r{round:02}_f{index:03}.mkv"),
                    None,
                )
            })
            .collect();
        files += rows.len();
        window_ms.push(timed_window(&store, &rows, 2_000 + round as i64));
        let after = media_rows(&data_dir, &lib.id);
        assert_eq!(
            after[&bulk_media.to_string()].item_revision,
            10 + round as i64,
            "bulk window {round} (100 files) must add exactly one reduction"
        );
    }
    let (db_after, wal_after) = library_bytes(&data_dir);

    window_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let windows = window_ms.len();
    let total: f64 = window_ms.iter().sum();
    let p50 = window_ms[windows / 2];
    let p95 = window_ms[(windows * 95) / 100];
    println!(
        "scan_window_cost_measurement: windows={windows} files={files} total_ms={total:.1} \
         p50_ms={p50:.2} p95_ms={p95:.2} max_ms={:.2} throughput_files_per_s={:.0}",
        window_ms[windows - 1],
        files as f64 / (total / 1000.0)
    );
    println!(
        "scan_window_cost_measurement: longest_transaction_ms={:.2} library_db_bytes={db_before}\
         ->{db_after} (delta {}) wal_bytes={wal_before}->{wal_after} (delta {}) rss_kb={:?}",
        window_ms[windows - 1],
        db_after as i64 - db_before as i64,
        wal_after as i64 - wal_before as i64,
        rss_kb()
    );

    // The first window creates each summary row (`item_revision = 1` on insert);
    // every later window that reduces that Media adds exactly one.
    let rows = media_rows(&data_dir, &lib.id);
    assert_eq!(rows.len(), 100);
    assert_eq!(rows[&bulk_media.to_string()].file_count, 1_010);
    assert_eq!(
        rows[&bulk_media.to_string()].item_revision,
        20,
        "1 insert + 9 incremental windows + 10 bulk windows = 20 reductions"
    );
    let others: Vec<&MediaRow> = rows
        .iter()
        .filter(|(media, _)| **media != bulk_media.to_string())
        .map(|(_, row)| row)
        .collect();
    assert!(others.iter().all(|row| row.file_count == 10));
    assert!(others.iter().all(|row| row.item_revision == 10));
}

/// Lock-wait diagnostic. Both writers use the same 5s busy timeout configured by
/// `Store::open`: a plain writer waits the other writer out, while the store's scan
/// window is measured under the same contention and its outcome is printed. A window
/// that fails under contention must have committed nothing, and the retry must then
/// converge to exactly one reduce for that window.
#[test]
fn window_commit_under_write_lock_contention() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let db = data_dir.join("library.db");
    let store = Store::open(&data_dir).unwrap();
    let lib = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &["/movies"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = fixture_media(0, MediaKind::Movie);
    store.insert_media(&media).unwrap();
    let rows = vec![scanned_row(media.id, "/movies/locked.mkv", None)];

    let plain = rusqlite::Connection::open(&db).unwrap();
    plain.busy_timeout(Duration::from_secs(5)).unwrap();
    plain
        .execute_batch("CREATE TABLE IF NOT EXISTS lock_probe (x INTEGER)")
        .unwrap();

    let release = hold_write_lock(&db, Duration::from_millis(60));
    let start = Instant::now();
    let plain_write = plain.execute("INSERT INTO lock_probe (x) VALUES (1)", []);
    let plain_wait = start.elapsed();
    release.join().unwrap();
    assert!(
        plain_write.is_ok(),
        "a plain writer must wait out the other writer, got {plain_write:?}"
    );
    assert!(
        plain_wait >= Duration::from_millis(40),
        "plain writer lock wait {plain_wait:?} must cover the ~60ms lock hold"
    );

    // Mechanism probe: a deferred transaction that has already read cannot upgrade to a
    // writer while another connection holds the lock. `insert_scanned_files` takes the
    // write lock at BEGIN (IMMEDIATE) so it waits instead.
    let deferred = rusqlite::Connection::open(&db).unwrap();
    deferred.busy_timeout(Duration::from_secs(5)).unwrap();
    deferred.execute_batch("BEGIN DEFERRED").unwrap();
    let read = deferred.query_row("SELECT count(*) FROM library_files", [], |row| {
        row.get::<_, i64>(0)
    });
    let release = hold_write_lock(&db, Duration::from_millis(60));
    let start = Instant::now();
    let upgrade = deferred.execute("INSERT INTO lock_probe (x) VALUES (2)", []);
    let upgrade_wait = start.elapsed();
    release.join().unwrap();
    let _ = deferred.execute_batch("ROLLBACK");
    assert!(read.is_ok(), "the deferred reader must read, got {read:?}");

    let release = hold_write_lock(&db, Duration::from_millis(60));
    let start = Instant::now();
    let outcome = store.insert_scanned_files(&rows, 1_000);
    let store_wait = start.elapsed();
    release.join().unwrap();
    println!(
        "window_commit_under_write_lock_contention: plain_writer_lock_wait_ms={} \
         deferred_read_then_write_ms={} deferred_outcome={upgrade:?} \
         store_window_under_contention_ms={} store_outcome={outcome:?}",
        plain_wait.as_millis(),
        upgrade_wait.as_millis(),
        store_wait.as_millis()
    );
    assert!(
        upgrade.is_err(),
        "probe: a deferred read-then-write cannot upgrade past another writer"
    );
    assert!(
        outcome.is_ok(),
        "C03: the IMMEDIATE window waits out the writer instead, got {outcome:?}"
    );
    let key = media.id.to_string();
    assert_eq!(count_in(&data_dir, "library_files", &lib.id), 1);
    assert_eq!(count_in(&data_dir, "file_probe_obligations", &lib.id), 1);
    let summaries = media_rows(&data_dir, &lib.id);
    assert_eq!(summaries[&key].file_count, 1);
    assert_eq!(
        summaries[&key].item_revision, 1,
        "one committed window reduces this Media once"
    );
}
