//! Checkpoints use the registered root identity even after its alias disappears.

use domain::{Media, MediaId, MediaKind};
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

#[test]
fn checkpoint_and_durable_versions_keep_the_recorded_alias_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    let alias = tmp.path().join("alias");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[alias.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
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
        })
        .unwrap();
    let display = alias.join("Movie.mkv");
    std::fs::write(&display, b"movie").unwrap();
    let row = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id,
            path: display.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let canonical = std::fs::canonicalize(&display)
        .unwrap()
        .display()
        .to_string();
    let display = display.display().to_string();
    store
        .record_scan_seen_stamps("operation", &[(display.clone(), 10, 5, None)])
        .unwrap();
    std::fs::remove_file(&alias).unwrap();

    let paths = vec![display.clone(), canonical.clone()];
    let versions = store.ledger_source_versions(&paths).unwrap();
    assert_eq!(versions.get(&display), Some(&(10, 5, None)));
    assert_eq!(versions.get(&canonical), versions.get(&display));
    let checkpoints = store.scan_seen_checkpoints("operation", &paths).unwrap();
    assert_eq!(checkpoints.get(&display), Some(&(10, 5, None, true)));
    assert_eq!(checkpoints.get(&canonical), checkpoints.get(&display));
    assert!(store.is_scan_seen("operation", &canonical).unwrap());

    // A stamp already observed outside the lock keeps updating the same file,
    // even if the mount/alias vanished before the DB phase.
    store
        .record_scan_seen_stamps("operation", &[(display.clone(), 20, 7, None)])
        .unwrap();
    assert_eq!(
        store.ledger_source_versions(&paths).unwrap()[&canonical],
        (20, 7, None)
    );
    assert_eq!(
        store
            .get_file_version(row.file_id)
            .unwrap()
            .unwrap()
            .revision,
        1
    );
}
