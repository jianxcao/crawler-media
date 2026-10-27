//! Watcher reads only the affected file or directory, in bounded windows.
use domain::{LedgerId, Media, MediaId, MediaKind};
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{FilePresence, LedgerPresenceMode, Store};

fn fixture() -> (tempfile::TempDir, Store, String, String, MediaId) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Watch",
            &["/watch"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id == library.id)
        .unwrap()
        .id;
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: "Watch".into(),
            original_title: None,
            aliases: vec![],
            year: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    (dir, store, library.id, root, media_id)
}

fn file(store: &Store, media_id: MediaId, path: &str) {
    publish_fixture_file(
        store,
        &FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id,
            path: path.into(),
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn event_pages_keep_path_boundaries_and_missing_timestamp() {
    let (_dir, store, library, _root, media_id) = fixture();
    for path in [
        "/watch/film/a.mkv",
        "/watch/film/sub/b.mkv",
        "/watch/film-extra/c.mkv",
    ] {
        file(&store, media_id, path);
    }
    let mut first = store.watcher_presence_page("/watch/film", None, 1).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].file.file.library_id.to_string(), library);
    first[0].presence = FilePresence::Missing;
    store
        .apply_ledger_presence(&first, LedgerPresenceMode::Verify, 42)
        .unwrap();
    let first_again = store.watcher_presence_page("/watch/film", None, 1).unwrap();
    assert_eq!(first_again[0].missing_at, Some(42));
    let cursor = first_again[0].path_key.clone();
    let second = store
        .watcher_presence_page("/watch/film", Some(&cursor), 1)
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_ne!(first_again[0].path_key, second[0].path_key);
    assert!(
        store
            .watcher_presence_page("/watch/film", Some(&second[0].path_key), 1)
            .unwrap()
            .is_empty()
    );
    let exact = store
        .watcher_presence_page("/watch/film/a.mkv", None, 100)
        .unwrap();
    assert_eq!(exact.len(), 1);
    assert_eq!(
        exact[0].file.file.path.to_string_lossy(),
        "/watch/film/a.mkv"
    );
    assert!(
        store
            .watcher_presence_page("/watch/film", None, 101)
            .is_err()
    );
}

#[test]
fn exact_path_and_descendants_continue_across_pages() {
    let (_dir, store, _library, _root, media_id) = fixture();
    file(&store, media_id, "/watch/film");
    file(&store, media_id, "/watch/film/a.mkv");
    let exact = store.watcher_presence_page("/watch/film", None, 1).unwrap();
    assert_eq!(exact[0].file.file.path.to_string_lossy(), "/watch/film");
    let child = store
        .watcher_presence_page("/watch/film", Some(&exact[0].path_key), 1)
        .unwrap();
    assert_eq!(child.len(), 1);
    assert_eq!(
        child[0].file.file.path.to_string_lossy(),
        "/watch/film/a.mkv"
    );
}

#[test]
fn event_path_work_does_not_grow_with_unrelated_files() {
    let (dir, store, library, root, media_id) = fixture();
    file(&store, media_id, "/watch/film/a.mkv");
    let mut db = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    let mut previous = 0;
    let mut baseline = None;
    for count in [100, 1000, 10_000] {
        let tx = db.transaction().unwrap();
        for n in previous..count {
            let path = format!("/watch/unrelated/{n}.mkv");
            tx.execute("INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,
                presence_state,publication_state,file_revision,source_version,quality_source,confidence)
                VALUES(?1,?2,?3,?4,'movie',?5,?5,'present','published',1,'v1','release','high')",
                rusqlite::params![LedgerId::new().to_string(), library, root, media_id.to_string(), path]).unwrap();
        }
        tx.commit().unwrap();
        previous = count;
        let (rows, vm, scans) = store
            .watcher_presence_page_with_work("/watch/film", None, 100)
            .unwrap();
        assert_eq!(rows.len(), 1);
        if let Some(expected) = baseline {
            assert_eq!((vm, scans), expected);
        } else {
            baseline = Some((vm, scans));
        }
    }
}
