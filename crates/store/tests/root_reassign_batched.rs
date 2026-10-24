//! A6: nested-root reassignment moves files in batches of ≤100, one transaction each,
//! and resumes from a durable cursor after a crash.
use domain::{Media, MediaId, MediaKind};
use store::Store;
use store::test_support::{
    FixtureFile, pending_root_reassignments, publish_fixture_file, set_root_reassign_batch_budget,
};

fn media(store: &Store) -> MediaId {
    let id = MediaId::new();
    store
        .insert_media(&Media {
            id,
            kind: MediaKind::Movie,
            title: "M".into(),
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

fn owned_by(store: &Store, library: &str) -> usize {
    store::test_support::library_file_count(store, library)
}

#[test]
fn nested_root_reassigns_in_bounded_batches_and_resumes_after_crash() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let parent = std::fs::canonicalize(tmp.path()).unwrap().join("p");
    let nested = parent.join("n");
    std::fs::create_dir_all(&nested).unwrap();
    let (parent_id, nested_id);
    {
        let store = Store::open(&data).unwrap();
        let lib = store
            .create_library(
                MediaKind::Movie,
                "P",
                &[parent.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        parent_id = lib.id.clone();
        for i in 0..250 {
            let path = nested.join(format!("m{i:03}.mkv"));
            std::fs::write(&path, b"x").unwrap();
            let m = media(&store);
            publish_fixture_file(
                &store,
                &FixtureFile {
                    media_id: m,
                    path: path.display().to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        assert_eq!(owned_by(&store, &parent_id), 250);
        // Crash after the first batch commits.
        set_root_reassign_batch_budget(Some(1));
        let lib = store
            .create_library(
                MediaKind::Movie,
                "N",
                &[nested.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        set_root_reassign_batch_budget(None);
        nested_id = lib.id.clone();
        assert_eq!(
            owned_by(&store, &nested_id),
            100,
            "one batch of 100 committed"
        );
        assert_eq!(owned_by(&store, &parent_id), 150);
        assert_eq!(
            pending_root_reassignments(&store).len(),
            1,
            "durable cursor kept"
        );
    }
    let store = Store::open(&data).unwrap();
    let batches = store.drain_root_reassignments().unwrap();
    assert_eq!(batches, 2, "resumes from the cursor: 100 + 50");
    assert_eq!(owned_by(&store, &nested_id), 250);
    assert_eq!(owned_by(&store, &parent_id), 0);
    assert!(pending_root_reassignments(&store).is_empty());
}

/// F02: one Media with files in two directories, past the 100-file batch, must keep
/// the episode NFO of a directory the first batch has not moved yet.
#[test]
fn batched_reassign_keeps_nfo_inputs_of_later_files() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let parent = std::fs::canonicalize(tmp.path()).unwrap().join("shows");
    let nested = parent.join("Show");
    let dir_a = nested.join("Season 01");
    let dir_b = nested.join("Season 02");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();
    let store = Store::open(&data).unwrap();
    let lib = store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[parent.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Show".into(),
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
    std::fs::write(
        nested.join("tvshow.nfo"),
        "<tvshow><title>Work</title><plot>Work plot</plot></tvshow>",
    )
    .unwrap();
    std::fs::write(
        dir_b.join("season.nfo"),
        "<season><seasonnumber>2</seasonnumber><title>Season two</title><plot>Season plot</plot></season>",
    )
    .unwrap();
    publish_season(&store, media_id, &dir_a, 1, 100);
    let episode_b = dir_b.join("Show.S02E01.mkv");
    std::fs::write(&episode_b, b"b").unwrap();
    std::fs::write(
        dir_b.join("Show.S02E01.nfo"),
        "<episodedetails><season>2</season><episode>1</episode><title>Later</title><plot>Later plot</plot></episodedetails>",
    )
    .unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id,
            path: episode_b.display().to_string(),
            season: Some(2),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    let before_episode = nfo_rows(&data, "episode");
    let before_work = nfo_rows(&data, "work");
    let before_season = nfo_rows(&data, "season");
    assert_eq!(before_episode.len(), 1, "the later episode input exists");
    assert_eq!(before_work.len(), 1, "the shared work input exists");
    assert_eq!(before_season.len(), 1, "the later season input exists");

    set_root_reassign_batch_budget(Some(1));
    let nested_lib = store
        .create_library(
            MediaKind::Tv,
            "Nested",
            &[nested.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    set_root_reassign_batch_budget(None);
    assert_eq!(owned_by(&store, &nested_lib.id), 100);
    assert_eq!(owned_by(&store, &lib.id), 1);
    assert_eq!(
        nfo_rows(&data, "episode"),
        before_episode,
        "the unmoved episode input survives the first batch"
    );
    drop(store);

    let store = Store::open(&data).unwrap();
    store.drain_root_reassignments().unwrap();
    assert_eq!(owned_by(&store, &nested_lib.id), 101);
    assert_eq!(owned_by(&store, &lib.id), 0);
    assert_eq!(nfo_rows(&data, "episode"), before_episode);
    assert_eq!(nfo_rows(&data, "work"), before_work);
    assert_eq!(nfo_rows(&data, "season"), before_season);
    assert_eq!(nfo_owner(&data, "episode"), vec![nested_lib.id.clone()]);
    assert_eq!(nfo_owner(&data, "season"), vec![nested_lib.id.clone()]);
    assert_eq!(
        nfo_owner(&data, "work"),
        vec![nested_lib.id.clone()],
        "the shared work input follows the files once the parent has none left"
    );
}

fn nfo_rows(data: &std::path::Path, scope: &str) -> Vec<(String, String, Option<String>)> {
    let db = rusqlite::Connection::open(data.join("library.db")).unwrap();
    let mut stmt = db
        .prepare(
            "SELECT locale, provenance_json, export_digest FROM library_metadata_inputs
             WHERE entity_scope = ?1 ORDER BY input_key",
        )
        .unwrap();
    stmt.query_map([scope], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// F03: freezing the destination must not confirm a cursor over files that stayed
/// in the parent. Resuming after the freeze finishes the move.
#[test]
fn frozen_destination_does_not_confirm_unmoved_files() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let parent = std::fs::canonicalize(tmp.path()).unwrap().join("p");
    let nested = parent.join("n");
    std::fs::create_dir_all(&nested).unwrap();
    let store = Store::open(&data).unwrap();
    let parent_lib = movie_library(&store, "Parent", &parent);
    let media_id = media(&store);
    for i in 0..150 {
        let path = nested.join(format!("m{i:03}.mkv"));
        std::fs::write(&path, b"x").unwrap();
        publish_fixture_file(
            &store,
            &FixtureFile {
                media_id,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
    }
    set_root_reassign_batch_budget(Some(1));
    let nested_lib = movie_library(&store, "Nested", &nested);
    set_root_reassign_batch_budget(None);
    assert_eq!(owned_by(&store, &nested_lib.id), 100);
    assert_eq!(owned_by(&store, &parent_lib.id), 50);
    let revision = store.library_config_revision(&nested_lib.id).unwrap();
    store
        .begin_library_reconfiguration(
            &store::library_operations::LibraryReconfiguration {
                operation_id: "locale-freeze".into(),
                library_id: nested_lib.id.parse().unwrap(),
                expected_revision: revision,
                root_paths: None,
                display_locale: Some("en-US".into()),
            },
            1,
        )
        .unwrap();
    store.drain_root_reassignments().unwrap();
    assert_eq!(
        pending_root_reassignments(&store).len(),
        1,
        "a frozen destination must keep the cursor"
    );
    assert_eq!(owned_by(&store, &parent_lib.id), 50);
    store
        .advance_library_operation("locale-freeze", 100, 2)
        .unwrap();
    store.drain_root_reassignments().unwrap();
    assert_eq!(owned_by(&store, &nested_lib.id), 150);
    assert_eq!(owned_by(&store, &parent_lib.id), 0);
    assert!(pending_root_reassignments(&store).is_empty());
}

fn publish_season(store: &Store, media_id: MediaId, dir: &std::path::Path, season: u32, count: u32) {
    for i in 0..count {
        let path = dir.join(format!("Show.S{season:02}E{i:03}.mkv"));
        std::fs::write(&path, b"a").unwrap();
        publish_fixture_file(
            store,
            &FixtureFile {
                media_id,
                path: path.display().to_string(),
                season: Some(season),
                episode: Some(i + 1),
                ..Default::default()
            },
        )
        .unwrap();
    }
}

fn movie_library(store: &Store, name: &str, root: &std::path::Path) -> store::Library {
    store
        .create_library(MediaKind::Movie, name, &[root.to_str().unwrap()], "everyone", true, &[])
        .unwrap()
}

fn nfo_owner(data: &std::path::Path, scope: &str) -> Vec<String> {
    let db = rusqlite::Connection::open(data.join("library.db")).unwrap();
    let mut stmt = db
        .prepare(
            "SELECT library_id FROM library_metadata_inputs WHERE entity_scope = ?1 ORDER BY input_key",
        )
        .unwrap();
    stmt.query_map([scope], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
