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

fn seed_cross_season_file() -> (
    tempfile::TempDir,
    store::Store,
    domain::UserId,
    MediaId,
    domain::LibraryFile,
) {
    use domain::{
        Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile,
        QualitySource, User, UserId, UserRole,
    };
    use store::library_write::FilePublication as StorePublication;

    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let store = store::Store::open(temp.path().join("data")).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id == library.id)
        .unwrap()
        .id;
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Cross Season".into(),
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
    let user_id = UserId::new();
    store
        .insert_user(&User {
            id: user_id,
            login: "viewer".into(),
            enabled: true,
            role: UserRole::Member,
        })
        .unwrap();
    let path = root.join("S01E12-S02E01.mkv");
    std::fs::write(&path, b"cross-season").unwrap();
    let file = LibraryFile {
        file_id: LedgerId::new(),
        library_id: library.id.parse().unwrap(),
        root_id,
        media_id,
        kind: MediaKind::Tv,
        path,
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(12),
        stat_known: true,
        created_ns: Some(1),
        modified_ns: Some(2),
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".into()),
        codec: Some("hevc".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: Some(1_000_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    let scope = WriteScope {
        library_id: file.library_id,
        config_revision: 0,
    };
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file,
                units: &[UnitKey::new(1, 12), UnitKey::new(2, 1)],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();
    (temp, store, user_id, media_id, file)
}

#[test]
fn cross_season_file_selects_real_lowest_covered_unit() {
    use domain::TypedItemId;

    let (_temp, store, user_id, media_id, file) = seed_cross_season_file();

    // Work scope: file appears once with its real lowest covered unit S01E12 (never S01E01).
    let work = store
        .playable_entries(user_id, &TypedItemId::Work(media_id))
        .unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].file.file_id, file.file_id);
    assert_eq!(work[0].unit, UnitKey::new(1, 12));
    store
        .issue_play_session(
            user_id,
            "web",
            work[0].file.file_id,
            media_id,
            work[0].unit,
            &work[0].file.version,
            2_000,
        )
        .unwrap();

    let s1 = store
        .playable_entries(
            user_id,
            &TypedItemId::Season {
                media: media_id,
                season: 1,
            },
        )
        .unwrap();
    assert_eq!(s1.len(), 1);
    assert_eq!(s1[0].unit, UnitKey::new(1, 12));

    let s2 = store
        .playable_entries(
            user_id,
            &TypedItemId::Season {
                media: media_id,
                season: 2,
            },
        )
        .unwrap();
    assert_eq!(s2.len(), 1);
    assert_eq!(s2[0].unit, UnitKey::new(2, 1));

    for covered in [UnitKey::new(1, 12), UnitKey::new(2, 1)] {
        let entries = store
            .playable_entries(
                user_id,
                &TypedItemId::Unit {
                    media: media_id,
                    key: covered,
                },
            )
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].unit, covered);
    }
    assert!(
        store
            .playable_entries(
                user_id,
                &TypedItemId::Unit {
                    media: media_id,
                    key: UnitKey::new(1, 1),
                },
            )
            .unwrap()
            .is_empty()
    );
}
