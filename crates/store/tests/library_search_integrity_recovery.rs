//! Public Integrity recovery and paged FTS orphan cleanup.
use domain::{LibraryId, Media, MediaId, MediaKind};
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    library: LibraryId,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let library = store
            .create_library(
                MediaKind::Movie,
                "Movies",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
            .parse()
            .unwrap();
        Self {
            dir,
            store,
            library,
        }
    }
    fn publish(&self, n: u64) -> MediaId {
        let id = format!("00000000-0000-0000-0000-{n:012}").parse().unwrap();
        self.store
            .insert_media(&Media {
                id,
                kind: MediaKind::Movie,
                title: format!("Needle Movie {n}"),
                year: None,
                original_title: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
                aliases: vec![],
            })
            .unwrap();
        let path = self.dir.path().join("movies").join(format!("{n}.mkv"));
        std::fs::write(&path, b"fixture").unwrap();
        publish_fixture_file(
            &self.store,
            &FixtureFile {
                media_id: id,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        id
    }
    fn finish(store: &Store, operation: &str, batch: usize) -> usize {
        for step in 1..100 {
            let status = store
                .advance_library_operation(operation, 2_000 + step, batch)
                .unwrap();
            assert_ne!(status.state, "failed", "{status:?}");
            if status.state == "succeeded" {
                return step as usize;
            }
        }
        panic!("finite fixture operation did not terminate")
    }
}

#[test]
fn integrity_partial_batch_survives_store_reopen_for_check_and_repair() {
    for repair in [false, true] {
        let f = Fixture::new();
        let expected: Vec<_> = (1..=4).map(|n| f.publish(n)).collect();
        if repair {
            let db = rusqlite::Connection::open(f.dir.path().join("data/library.db")).unwrap();
            // Every card requires repair, including keys beyond the first batch.
            db.execute("DELETE FROM library_search_fts", []).unwrap();
        }
        f.store
            .begin_library_integrity(f.library, repair, "recover", 1_000)
            .unwrap();
        let status = f
            .store
            .advance_library_operation("recover", 1_001, 1)
            .unwrap();
        assert_eq!(
            status.state, "applying",
            "fixture must stop before completion"
        );
        let Fixture {
            dir,
            store,
            library,
        } = f;
        drop(store);
        let reopened = Store::open(dir.path().join("data")).unwrap();
        assert!(
            Fixture::finish(&reopened, "recover", 1) >= 3,
            "remaining card pages must resume after the durable first cursor"
        );
        let report = reopened.library_integrity_report("recover").unwrap();
        assert_eq!(
            report.has_issues(),
            repair,
            "repair retains damage report; clean check must stay clean"
        );
        let actual: Vec<_> = reopened
            .search_library_items(&[library], "needle", None, None)
            .unwrap()
            .items
            .into_iter()
            .map(|row| row.media_id)
            .collect();
        assert_eq!(actual, expected);
        reopened
            .begin_library_integrity(library, false, "verify", 3_000)
            .unwrap();
        Fixture::finish(&reopened, "verify", 1);
        assert!(
            !reopened
                .library_integrity_report("verify")
                .unwrap()
                .has_issues(),
            "resumed repair must leave all postings authoritative"
        );
    }
}

#[test]
fn integrity_repairs_more_orphan_postings_than_one_cleanup_batch() {
    let f = Fixture::new();
    let wanted = f.publish(1);
    {
        let db = rusqlite::Connection::open(f.dir.path().join("data/library.db")).unwrap();
        for rowid in 10_000..10_009 {
            db.execute("INSERT INTO library_search_fts(rowid,search_text,people) VALUES(?1,'orphan phantom','wrong actor')",[rowid]).unwrap();
        }
    }
    f.store
        .begin_library_integrity(f.library, false, "detect-orphans", 1_000)
        .unwrap();
    Fixture::finish(&f.store, "detect-orphans", 2);
    assert!(
        f.store
            .library_integrity_report("detect-orphans")
            .unwrap()
            .has_issues()
    );
    f.store
        .begin_library_integrity(f.library, true, "repair-orphans", 2_000)
        .unwrap();
    let steps = Fixture::finish(&f.store, "repair-orphans", 2);
    assert!(
        steps >= 5,
        "nine orphan docs with cleanup batch two require at least five commits; got {steps}"
    );
    let hits = f
        .store
        .search_library_items(&[f.library], "needle", None, None)
        .unwrap();
    assert_eq!(hits.items.len(), 1);
    assert_eq!(hits.items[0].media_id, wanted);
    // A second public check must detect zero leftovers; successful completion
    // alone could otherwise hide a cleanup loop that skipped its final page.
    f.store
        .begin_library_integrity(f.library, false, "verify-orphans", 3_000)
        .unwrap();
    Fixture::finish(&f.store, "verify-orphans", 2);
    assert!(
        !f.store
            .library_integrity_report("verify-orphans")
            .unwrap()
            .has_issues()
    );
    let db = rusqlite::Connection::open(f.dir.path().join("data/library.db")).unwrap();
    let documents: i64 = db
        .query_row("SELECT COUNT(*) FROM library_search_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        documents, 1,
        "cleanup preserves the authoritative card and removes every orphan"
    );
}
