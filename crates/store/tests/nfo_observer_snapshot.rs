use domain::{FileVersion, MediaKind, UnitKey};
use store::Store;

#[test]
fn observer_keeps_its_root_identity_after_an_alias_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let old_root = tmp.path().join("old");
    let new_root = tmp.path().join("new");
    for root in [&old_root, &new_root] {
        std::fs::create_dir(root).unwrap();
        std::fs::write(
            root.join("movie.nfo"),
            "<movie><title>Movie</title></movie>",
        )
        .unwrap();
    }
    let alias = tmp.path().join("alias");
    std::os::unix::fs::symlink(&old_root, &alias).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[alias.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let observer = store.nfo_input_observer();
    store
        .update_library(&library.id, None, Some(&[new_root.to_str().unwrap()]), None)
        .unwrap();
    let version = FileVersion {
        revision: 1,
        source_version: "observed-source".into(),
    };
    let observe = |observer: &store::metadata::nfo_input::NfoInputObserver| {
        observer.observe(
            &alias.join("Movie.mkv"),
            MediaKind::Movie,
            &[UnitKey::WHOLE],
            "zh-CN",
            &version,
            0,
        )
    };
    let old_key = std::fs::canonicalize(old_root).unwrap().join("movie.nfo");
    // The next plan does not inherit an alias that was removed from configuration.
    let new_key = alias.join("movie.nfo");
    assert!(
        observe(&observer)
            .iter()
            .any(|row| row.input_key == old_key.to_string_lossy())
    );
    assert!(
        observe(&store.nfo_input_observer())
            .iter()
            .any(|row| row.input_key == new_key.to_string_lossy())
    );
}

#[test]
fn reconciliation_rejects_a_renamed_file_with_the_same_source_version() {
    use domain::{Media, MediaId};
    use store::test_support::{FixtureFile, publish_fixture_file};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir(&root).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
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
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Movie".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let path = root.join("Movie.mkv");
    std::fs::write(&path, b"movie").unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media.id,
            path: path.to_string_lossy().into_owned(),
            ..Default::default()
        },
    )
    .unwrap();
    std::fs::write(
        root.join("movie.nfo"),
        "<movie><title>Stale</title></movie>",
    )
    .unwrap();
    let observed = store
        .plan_nfo_input_reconcile(&path)
        .unwrap()
        .unwrap()
        .observe();
    let other = Store::open(tmp.path().join("data")).unwrap();
    let destination = root.join("Renamed.mkv");
    other
        .rename_file_within_root(file.file_id, &file.version, &path, &destination)
        .unwrap();
    assert!(
        !store.apply_nfo_input_reconcile(observed).unwrap(),
        "old locator observations must not publish after rename"
    );
}
