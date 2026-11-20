use std::path::PathBuf;

use domain::{Media, MediaId, MediaKind};
use store::Store;
use store::library_operations::LibraryReconfiguration;
use store::test_support::{FixtureFile, publish_fixture_file};

fn create_media(store: &Store, title: &str) -> MediaId {
    let id = MediaId::new();
    store
        .insert_media(&Media {
            id,
            kind: MediaKind::Movie,
            title: title.into(),
            year: Some(2020),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    id
}

fn finish_reconfiguration(store: &Store, operation_id: &str) {
    for step in 0..100 {
        let status = store
            .advance_library_operation(operation_id, 2_000 + step, 3)
            .unwrap();
        if status.state == "succeeded" {
            return;
        }
        assert_ne!(status.state, "failed", "{:?}", status.error);
    }
    panic!("reconfiguration did not finish");
}

fn reconfigure(store: &Store, library_id: &str, roots: Vec<PathBuf>, operation_id: &str) {
    store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: operation_id.into(),
                library_id: library_id.parse().unwrap(),
                expected_revision: store.library_config_revision(library_id).unwrap(),
                root_paths: Some(roots),
                display_locale: None,
            },
            1_000,
        )
        .unwrap();
    finish_reconfiguration(store, operation_id);
}

#[test]
fn changing_root_order_does_not_update_nfo_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let root_a = temp.path().join("a");
    let root_b = temp.path().join("b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root_a.to_str().unwrap(), root_b.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    for (root, prefix) in [(&root_a, "a"), (&root_b, "b")] {
        for index in 0..6 {
            let dir = root.join(format!("{prefix}-{index}"));
            std::fs::create_dir_all(&dir).unwrap();
            let video = dir.join("movie.mkv");
            std::fs::write(&video, b"video").unwrap();
            std::fs::write(dir.join("movie.nfo"), "<movie><title>Movie</title></movie>").unwrap();
            let media_id = create_media(&store, &format!("{prefix}-{index}"));
            publish_fixture_file(
                &store,
                &FixtureFile {
                    media_id,
                    path: video.display().to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
    }

    let db_path = temp.path().join("data/library.db");
    let db = rusqlite::Connection::open(db_path).unwrap();
    let (before_count, before_revision_sum): (i64, i64) = db
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(revision), 0) FROM library_metadata_inputs WHERE library_id=?1",
            [&library.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(before_count, 12);
    db.execute_batch(
        "CREATE TRIGGER count_nfo_root_updates AFTER UPDATE OF root_id ON library_metadata_inputs
         BEGIN UPDATE library_metadata_inputs SET revision=revision+1
           WHERE library_id=NEW.library_id AND media_id=NEW.media_id
             AND entity_scope=NEW.entity_scope AND season=NEW.season AND episode=NEW.episode
             AND locale=NEW.locale AND input_key=NEW.input_key; END;",
    )
    .unwrap();

    reconfigure(
        &store,
        &library.id,
        vec![root_b.clone(), root_a.clone()],
        "swap-root-order",
    );

    let (after_count, after_revision_sum): (i64, i64) = db
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(revision), 0) FROM library_metadata_inputs WHERE library_id=?1",
            [&library.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(after_count, before_count);
    assert_eq!(
        after_revision_sum - before_revision_sum,
        0,
        "ordering does not cause diagnostic NFO row updates"
    );
}

#[test]
fn adding_nested_root_moves_file_and_nfo_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("shows");
    let nested = parent.join("anime");
    std::fs::create_dir_all(&nested).unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[parent.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let video = nested.join("movie.mkv");
    std::fs::write(&video, b"video").unwrap();
    std::fs::write(
        nested.join("movie.nfo"),
        "<movie><title>Anime</title></movie>",
    )
    .unwrap();
    let media_id = create_media(&store, "Anime");
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id,
            path: video.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();

    reconfigure(
        &store,
        &library.id,
        vec![parent.clone(), nested.clone()],
        "add-nested-root",
    );

    let db = rusqlite::Connection::open(temp.path().join("data/library.db")).unwrap();
    let file_owner: String = db
        .query_row(
            "SELECT root_id FROM library_files WHERE media_id=?1",
            [media_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let input_owner: String = db
        .query_row(
            "SELECT root_id FROM library_metadata_inputs WHERE input_key LIKE '%movie.nfo'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(file_owner, input_owner);
    let nested_owner = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id == library.id && root.path == nested)
        .unwrap()
        .id;
    assert_eq!(file_owner, nested_owner);
}
