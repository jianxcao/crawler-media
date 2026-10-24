use std::collections::BTreeMap;
use std::path::Path;

use store::ScannedLedger;

use super::*;

// ---------------------------------------------------------------------------
// Scan commit window (Task 19, store side).
//
// The seams asserted below are persisted rows of `library.db`, read back through a
// separate connection:
//   * `library_items` — one summary row per (library, media): the affected-Media set,
//     `item_revision` (one reduce pass for that Media == +1), file and episode counts;
//   * `library_files` / `file_probe_obligations` / `library_units` — the per-file facts
//     that must exist exactly once per committed window.
// No private helper, no production SQL text and no log line is asserted.
// ---------------------------------------------------------------------------

/// One fixture scan row. `unit` sets the TV coverage unit; `None` is a whole-unit row.
pub(crate) fn scanned_row(
    media_id: MediaId,
    path: &str,
    unit: Option<(u32, u32)>,
) -> ScannedLedger {
    let units = match unit {
        Some((season, episode)) => vec![UnitKey::new(season as i32, episode as i32)],
        None => vec![UnitKey::WHOLE],
    };
    ScannedLedger {
        file_id: LedgerId::new(),
        media_id,
        path: path.to_string(),
        units,
        source_version: "fixture-v1".into(),
        source_digest: None,
        size_bytes: None,
        created_ns: None,
        modified_ns: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        artwork: Default::default(),
        observations: Vec::new(),
    }
}

pub(crate) fn fixture_media(index: usize, kind: MediaKind) -> Media {
    Media {
        id: MediaId::new(),
        kind,
        title: format!("Fixture {index:03}"),
        year: Some(2020),
        original_title: None,
        tmdb_id: Some(format!("fixture-{index}")),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: vec![],
    }
}

pub(crate) fn insert_medias(store: &Store, count: usize, kind: MediaKind) -> Vec<MediaId> {
    (0..count)
        .map(|index| {
            let media = fixture_media(index, kind);
            store.insert_media(&media).unwrap();
            media.id
        })
        .collect()
}

/// A committed window: one new whole-unit file for every Media in `medias`.
fn window_one_file_each(medias: &[MediaId], prefix: &str) -> Vec<ScannedLedger> {
    medias
        .iter()
        .map(|media| scanned_row(*media, &format!("/{prefix}/m{media}/new.mkv"), None))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MediaRow {
    pub(crate) item_revision: i64,
    pub(crate) file_count: i64,
    pub(crate) episodes: i64,
}

/// Persisted `library_items` rows of one library, keyed by media id.
pub(crate) fn media_rows(data_dir: &Path, library_id: &str) -> BTreeMap<String, MediaRow> {
    let conn = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT media_id, item_revision, file_count, distinct_episode_count
             FROM library_items WHERE library_id = ?1",
        )
        .unwrap();
    let rows = stmt
        .query_map([library_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                MediaRow {
                    item_revision: row.get(1)?,
                    file_count: row.get(2)?,
                    episodes: row.get(3)?,
                },
            ))
        })
        .unwrap()
        .collect::<Result<BTreeMap<_, _>, _>>()
        .unwrap();
    rows
}

pub(crate) fn count_in(data_dir: &Path, table: &str, library_id: &str) -> i64 {
    let sql = match table {
        "library_files" => "SELECT COUNT(*) FROM library_files WHERE library_id = ?1",
        "library_units" => "SELECT COUNT(*) FROM library_units WHERE library_id = ?1",
        "file_probe_obligations" => {
            "SELECT COUNT(*) FROM file_probe_obligations o
             JOIN library_files f ON f.file_id = o.file_id WHERE f.library_id = ?1"
        }
        other => panic!("unknown fixture table {other}"),
    };
    let conn = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
    conn.query_row(sql, [library_id], |row| row.get(0)).unwrap()
}

fn count_path(data_dir: &Path, path: &str) -> i64 {
    let conn = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM library_files WHERE display_path = ?1",
        [path],
        |row| row.get(0),
    )
    .unwrap()
}

/// Invariant: N files of one Media committed in one window produce exactly one
/// reduction (one summary write) for that Media — not N.
#[test]
fn one_window_reduces_a_single_media_once_not_once_per_file() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();
    let lib = store
        .create_library(MediaKind::Tv, "Shows", &["/shows"], "everyone", true, &[])
        .unwrap();
    let series = fixture_media(0, MediaKind::Tv);
    store.insert_media(&series).unwrap();

    // One scan window: 100 distinct episode files of the same Media.
    let rows: Vec<ScannedLedger> = (1..=100)
        .map(|episode| {
            scanned_row(
                series.id,
                &format!("/shows/s01e{episode:03}.mkv"),
                Some((1, episode)),
            )
        })
        .collect();
    assert_eq!(
        rows.len(),
        store::MAX_SCAN_BATCH,
        "window is one full batch"
    );
    store.insert_scanned_files(&rows, 1_000).unwrap();

    let key = series.id.to_string();
    let summaries = media_rows(&data_dir, &lib.id);
    assert_eq!(
        summaries.len(),
        1,
        "one window over one Media must touch exactly one summary row"
    );
    let summary = summaries.get(&key).expect("summary row");
    assert_eq!(
        summary.item_revision, 1,
        "100 files of one Media in one window = 1 reduce pass, got {}",
        summary.item_revision
    );
    assert_eq!(summary.file_count, 100);
    assert_eq!(summary.episodes, 100);
    assert_eq!(count_in(&data_dir, "library_files", &lib.id), 100);
    assert_eq!(
        count_in(&data_dir, "file_probe_obligations", &lib.id),
        100,
        "every file in the window still gets its probe obligation"
    );
    assert_eq!(count_in(&data_dir, "library_units", &lib.id), 100);
}

/// Invariant: the work committed in one window does not grow with the number of files
/// already stored for the affected Media. Two libraries seeded with the same 100 Media
/// (one holding 1,000 pre-existing files, one empty) take the same-shaped window and
/// must do the same reduction work.
#[test]
fn window_work_does_not_grow_with_pre_existing_library_files() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();
    let dense = store
        .create_library(MediaKind::Tv, "Dense", &["/dense"], "everyone", true, &[])
        .unwrap();
    let sparse = store
        .create_library(MediaKind::Tv, "Sparse", &["/sparse"], "everyone", true, &[])
        .unwrap();
    let medias = insert_medias(&store, 100, MediaKind::Tv);

    // Pre-existing: 10 windows of one file per Media in /dense = 1,000 stored files.
    for round in 1..=10u32 {
        let rows: Vec<ScannedLedger> = medias
            .iter()
            .map(|media| {
                scanned_row(
                    *media,
                    &format!("/dense/m{media}/s01e{round:02}.mkv"),
                    Some((1, round)),
                )
            })
            .collect();
        store
            .insert_scanned_files(&rows, 1_000 + round as i64)
            .unwrap();
    }
    let before_dense = media_rows(&data_dir, &dense.id);
    assert_eq!(count_in(&data_dir, "library_files", &dense.id), 1_000);
    assert!(
        before_dense.values().all(|row| row.file_count == 10),
        "pre-existing window facts must be committed"
    );

    // The identical window shape in both libraries: one new file per Media.
    let dense_rows = window_one_file_each(&medias, "dense");
    let sparse_rows = window_one_file_each(&medias, "sparse");
    store.insert_scanned_files(&dense_rows, 2_000).unwrap();
    store.insert_scanned_files(&sparse_rows, 2_001).unwrap();

    let after_dense = media_rows(&data_dir, &dense.id);
    let after_sparse = media_rows(&data_dir, &sparse.id);
    let dense_ids: Vec<&String> = after_dense.keys().collect();
    let sparse_ids: Vec<&String> = after_sparse.keys().collect();
    assert_eq!(
        dense_ids, sparse_ids,
        "the affected-Media set must be the window's Media, not the library's files"
    );
    assert_eq!(after_dense.len(), 100);
    assert_eq!(
        dense_ids.len(),
        medias.len(),
        "every Media in the window is reduced once"
    );

    let dense_deltas: Vec<i64> = after_dense
        .iter()
        .map(|(media, row)| row.item_revision - before_dense[media].item_revision)
        .collect();
    assert!(
        dense_deltas.iter().all(|delta| *delta == 1),
        "each affected Media is reduced exactly once per window despite 10 pre-existing \
         files, got deltas {dense_deltas:?}"
    );
    let sparse_revisions: Vec<i64> = after_sparse.values().map(|row| row.item_revision).collect();
    assert!(
        sparse_revisions.iter().all(|revision| *revision == 1),
        "the empty library needs the same single reduction per Media, got {sparse_revisions:?}"
    );
    assert_eq!(
        dense_deltas.iter().sum::<i64>(),
        sparse_revisions.iter().sum::<i64>(),
        "identical windows must commit identical reduction work"
    );

    assert!(after_dense.values().all(|row| row.file_count == 11));
    assert!(after_sparse.values().all(|row| row.file_count == 1));
    assert_eq!(count_in(&data_dir, "library_files", &dense.id), 1_100);
    assert_eq!(count_in(&data_dir, "library_files", &sparse.id), 100);
}

/// Invariant: one window accepts at most `MAX_SCAN_BATCH` (100) files, and a rejected
/// window writes nothing.
#[test]
fn one_window_accepts_at_most_one_hundred_files() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
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
    assert_eq!(
        store::MAX_SCAN_BATCH,
        100,
        "the scan commit window is the fixed 100-file bound"
    );

    let mut rows: Vec<ScannedLedger> = (0..100)
        .map(|index| scanned_row(media.id, &format!("/movies/ok_{index:03}.mkv"), None))
        .collect();
    store.insert_scanned_files(&rows, 1_000).unwrap();
    assert_eq!(count_in(&data_dir, "library_files", &lib.id), 100);

    // A 101-file window of files that are all new: rejected whole, nothing written.
    rows = (0..101)
        .map(|index| scanned_row(media.id, &format!("/movies/overflow_{index:03}.mkv"), None))
        .collect();
    let error = store.insert_scanned_files(&rows, 2_000).unwrap_err();
    assert!(
        matches!(error, store::StoreError::Invalid(_)),
        "101 files in one window must be rejected, got {error:?}"
    );

    let key = media.id.to_string();
    assert_eq!(count_in(&data_dir, "library_files", &lib.id), 100);
    assert_eq!(count_in(&data_dir, "file_probe_obligations", &lib.id), 100);
    assert_eq!(count_path(&data_dir, "/movies/overflow_000.mkv"), 0);
    assert_eq!(count_path(&data_dir, "/movies/overflow_100.mkv"), 0);
    assert_eq!(media_rows(&data_dir, &lib.id)[&key].item_revision, 1);
}

/// A window that fails mid-way must leave facts and summary at the previous state, and a
/// retry after reopening the Store must converge to one reduction for the whole window.
#[test]
fn failed_window_leaves_facts_and_summary_consistent_after_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
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

    let window = |prefix: &str| -> Vec<ScannedLedger> {
        (0..100)
            .map(|index| scanned_row(media.id, &format!("/movies/{prefix}_{index:03}.mkv"), None))
            .collect()
    };
    store.insert_scanned_files(&window("pre"), 1_000).unwrap();
    assert_eq!(count_in(&data_dir, "library_files", &lib.id), 100);

    // Block one file in the middle of the next window.
    let blocked = "/movies/window_050.mkv";
    let conn = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
    conn.execute_batch(&format!(
        "CREATE TRIGGER block_mid_window BEFORE INSERT ON library_files
         WHEN NEW.display_path = '{blocked}'
         BEGIN SELECT RAISE(ABORT, 'blocked mid-window'); END"
    ))
    .unwrap();
    drop(conn);

    let error = store
        .insert_scanned_files(&window("window"), 2_000)
        .unwrap_err();
    assert!(
        matches!(
            error,
            store::StoreError::Sqlite(_) | store::StoreError::Invalid(_)
        ),
        "the blocked window must fail, got {error:?}"
    );

    // Rolled back: no fact, obligation or summary from the failed window survives.
    let key = media.id.to_string();
    assert_eq!(count_in(&data_dir, "library_files", &lib.id), 100);
    assert_eq!(count_in(&data_dir, "file_probe_obligations", &lib.id), 100);
    assert_eq!(count_path(&data_dir, blocked), 0);
    let after_failure = media_rows(&data_dir, &lib.id);
    assert_eq!(after_failure[&key].file_count, 100);
    assert_eq!(after_failure[&key].item_revision, 1);

    // Restart, remove the blocker, retry the identical window.
    drop(store);
    let store = Store::open(&data_dir).unwrap();
    let conn = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
    conn.execute_batch("DROP TRIGGER block_mid_window").unwrap();
    drop(conn);
    store
        .insert_scanned_files(&window("window"), 3_000)
        .unwrap();

    assert_eq!(count_in(&data_dir, "library_files", &lib.id), 200);
    assert_eq!(count_in(&data_dir, "file_probe_obligations", &lib.id), 200);
    assert_eq!(count_path(&data_dir, blocked), 1);
    let after_retry = media_rows(&data_dir, &lib.id);
    assert_eq!(after_retry[&key].file_count, 200);
    assert_eq!(
        after_retry[&key].item_revision, 2,
        "one reduce per committed window; the failed window contributed none"
    );
}
