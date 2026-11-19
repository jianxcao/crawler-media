use domain::{Media, MediaId, MediaKind, UnitKey};
use library::TransferMode;
use store::library_write::WriteScope;
use store::test_support::{FixtureFile, publish_fixture_file};

#[test]
fn reserved_episode_becomes_playable_only_after_publication() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let store = store::Store::open(temp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[root.to_str().unwrap()],
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
            title: "Episodes".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let source = root.join("S01E01.mkv");
    std::fs::write(&source, b"media").unwrap();
    let first = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id,
            path: source.display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    let scope = WriteScope {
        library_id: first.library_id,
        config_revision: store
            .library_config_revision(&first.library_id.to_string())
            .unwrap(),
    };
    let mut candidate = store.file_coverage_by_id(first.file_id).unwrap().unwrap();
    candidate.file.file_id = domain::LedgerId::new();
    candidate.file.path = root.join("S01E02.mkv");
    candidate.path_key = candidate.file.path.display().to_string();
    candidate.units = vec![UnitKey::new(1, 2)];
    let reserved = store
        .reserve_transfer(&scope, &candidate, &source, TransferMode::Copy, 1_000)
        .unwrap();

    let before = store
        .file_coverage_in_library_season(
            scope.library_id,
            media_id,
            1,
            store::FileReadScope::Playable,
        )
        .unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].file.file_id, first.file_id);

    std::fs::copy(&source, &reserved.file.path).unwrap();
    store
        .finish_transfer(&scope, &reserved, &reserved.file.version, 2_000)
        .unwrap();
    let after = store
        .file_coverage_in_library_season(
            scope.library_id,
            media_id,
            1,
            store::FileReadScope::Playable,
        )
        .unwrap();
    assert_eq!(after.len(), 2);
    assert!(
        after
            .iter()
            .any(|row| row.file.file_id == reserved.file.file_id)
    );
}
