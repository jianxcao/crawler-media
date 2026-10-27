//! Public cleanup/reassignment work stays independent of unrelated history size.
use domain::{Media, MediaId, MediaKind};
use rusqlite::{Connection, params};
use store::stats::StatsScope;
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{Store, library_write::ReassignFiles};

fn media(store: &Store) -> MediaId {
    let id = MediaId::new();
    store
        .insert_media(&Media {
            id,
            kind: MediaKind::Movie,
            title: "Cleanup".into(),
            original_title: None,
            year: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    id
}

struct Fixture {
    _temp: tempfile::TempDir,
    store: Store,
    db: Connection,
    file: domain::LibraryFile,
    to: MediaId,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("data")).unwrap();
        let root = temp.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        store
            .create_library(
                MediaKind::Movie,
                "Movies",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let from = media(&store);
        let to = media(&store);
        let file = publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: from,
                path: root.join("movie.mkv").to_string_lossy().into_owned(),
                ..Default::default()
            },
        )
        .unwrap();
        let db = Connection::open(temp.path().join("data/library.db")).unwrap();
        Self {
            _temp: temp,
            store,
            db,
            file,
            to,
        }
    }
}

#[test]
fn deleting_one_file_does_not_scan_unrelated_fingerprint_attempts() {
    for history in [100, 1000, 10_000] {
        let f = Fixture::new();
        f.db.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?1)
            INSERT INTO fingerprint_attempts SELECT 'attempt-'||i,'job','other','intro',0,1,'capture',0,1,'ok',NULL,'{}' FROM n", [history]).unwrap();
        let (removed, stats) = f.store.with_stats(StatsScope::Library, || {
            f.store.delete_files(f.file.library_id, &[f.file.file_id])
        });
        assert_eq!(removed.unwrap(), 1);
        assert!(
            f.store
                .file_coverage_by_id(f.file.file_id)
                .unwrap()
                .is_none()
        );
        let retained: i64 =
            f.db.query_row("SELECT count(*) FROM fingerprint_attempts", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(retained, history);
        println!("delete history={history}: {stats:?}");
        assert!(
            stats.fullscan_steps < 50,
            "unrelated attempts scanned: {stats:?}"
        );
    }
}

#[test]
fn reassigning_one_file_does_not_scan_unrelated_session_ids() {
    for history in [100, 1000, 10_000] {
        let f = Fixture::new();
        f.db.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?1)
            INSERT INTO playback_session_ids SELECT 'user','device','session-'||i,'item','media',-1,-1,'other',1,'v1','library',0,'full_file',0,i,'active' FROM n", [history]).unwrap();
        f.db.execute("INSERT INTO playback_session_ids VALUES ('user','device','target','item',?1,-1,-1,?2,1,'v1',?3,0,'full_file',0,0,'active')",
            params![f.file.media_id.to_string(), f.file.file_id.to_string(), f.file.library_id.to_string()]).unwrap();
        let request = ReassignFiles {
            files: vec![(f.file.file_id, f.file.version.clone())],
            from: f.file.media_id,
            to: f.to,
            expected_media_revision: 0,
        };
        let (moved, stats) = f.store.with_stats(StatsScope::Library, || {
            f.store.reassign_files(&request, 1000)
        });
        assert_eq!(moved.unwrap(), 1);
        assert_eq!(
            f.store
                .file_coverage_by_id(f.file.file_id)
                .unwrap()
                .unwrap()
                .file
                .media_id,
            f.to
        );
        let closed: String =
            f.db.query_row(
                "SELECT state FROM playback_session_ids WHERE play_session_id='target'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(closed, "closed");
        println!("reassign history={history}: {stats:?}");
        assert!(
            stats.fullscan_steps < 50,
            "unrelated sessions scanned: {stats:?}"
        );
    }
}

#[test]
fn retention_window_does_not_scan_live_operations_or_sort_all_terminal_history() {
    for history in [100, 1000, 10_000] {
        let f = Fixture::new();
        f.db.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?1)
            INSERT INTO media_operations SELECT 'live-'||i,'merge','frozen','s','l','{}',NULL,NULL,0,0 FROM n", [history]).unwrap();
        f.db.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?1)
            INSERT INTO library_operations SELECT 'done-'||i,'library','roots','succeeded',0,'{}',NULL,NULL,0,i FROM n", [history]).unwrap();
        let (result, stats) = f.store.with_stats(StatsScope::Library, || {
            f.store.prune_terminal_operations(20_000, 10, false)
        });
        let result = result.unwrap();
        assert_eq!(result.deleted, 10);
        assert_eq!(
            f.db.query_row("SELECT count(*) FROM media_operations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            history
        );
        println!("retention history={history}: {stats:?}");
        assert!(
            stats.fullscan_steps < 50,
            "retention scanned unrelated/live rows: {stats:?}"
        );
        assert_eq!(
            stats.sort_operations, 0,
            "terminal window should seek age order"
        );
    }
}
