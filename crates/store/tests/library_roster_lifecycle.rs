use domain::{LedgerId, Media, MediaId, MediaKind, UnitKey};
use store::{
    Store,
    library_write::{FilePublication, WriteScope},
    test_support::{FixtureFile, publish_fixture_file},
};

fn totals(store: &Store, id: &str) -> (u64, u64, u64, u64) {
    let s = store.library_roster_stats(id).unwrap();
    (
        s.item_count,
        s.file_count,
        s.total_size_bytes,
        s.strm_file_count,
    )
}
#[test]
fn roster_stats_follow_publication_rollback_file_deletion_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let lib = store
        .create_library(
            MediaKind::Movie,
            "Lifecycle",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = MediaId::new();
    store
        .insert_media(&Media {
            id: media,
            kind: MediaKind::Movie,
            title: "Roster lifecycle".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let mut files = Vec::new();
    for (name, bytes) in [("first.mkv", 10), ("second.mkv", 20)] {
        let path = root.join(name);
        std::fs::write(&path, vec![b'x'; bytes]).unwrap();
        let mut file = publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        file.size_bytes = Some(bytes as u64);
        file.stat_known = true;
        store
            .publish_files(
                &WriteScope {
                    library_id: file.library_id,
                    config_revision: 0,
                },
                &[FilePublication {
                    file: &file,
                    units: &[UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                }],
                1000,
            )
            .unwrap();
        files.push(file);
    }
    assert_eq!(totals(&store, &lib.id), (1, 2, 30, 0));
    let mut good = files[0].clone();
    good.file_id = LedgerId::new();
    good.path = root.join("rollback-good.mkv");
    good.size_bytes = Some(99);
    let mut rejected = good.clone();
    rejected.file_id = LedgerId::new();
    rejected.path = root.join("rollback-rejected.mkv");
    let db = rusqlite::Connection::open(dir.path().join("data/library.db")).unwrap();
    db.execute_batch(&format!("CREATE TRIGGER reject_roster_publication BEFORE INSERT ON library_files WHEN NEW.file_id='{}' BEGIN SELECT RAISE(ABORT,'publication rollback fault'); END",rejected.file_id)).unwrap();
    let units = [UnitKey::WHOLE];
    let publications: Vec<_> = [&good, &rejected]
        .into_iter()
        .map(|file| FilePublication {
            file,
            units: &units,
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        })
        .collect();
    assert!(
        store
            .publish_files(
                &WriteScope {
                    library_id: files[0].library_id,
                    config_revision: 0
                },
                &publications,
                1000
            )
            .is_err()
    );
    assert!(store.published_files(&[good.file_id]).unwrap().is_empty());
    assert_eq!(totals(&store, &lib.id), (1, 2, 30, 0));
    db.execute_batch("DROP TRIGGER reject_roster_publication")
        .unwrap();
    store
        .delete_files(files[0].library_id, &[files[0].file_id])
        .unwrap();
    assert_eq!(totals(&store, &lib.id), (1, 1, 20, 0));
    drop(store);
    let reopened = Store::open(dir.path().join("data")).unwrap();
    assert_eq!(totals(&reopened, &lib.id), (1, 1, 20, 0));
    reopened
        .delete_files(files[1].library_id, &[files[1].file_id])
        .unwrap();
    assert_eq!(totals(&reopened, &lib.id), (0, 0, 0, 0));
    drop(reopened);
    assert_eq!(
        totals(&Store::open(dir.path().join("data")).unwrap(), &lib.id),
        (0, 0, 0, 0)
    );
}
