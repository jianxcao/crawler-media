use domain::{Media, MediaId, MediaKind, UnitKey};
use store::{
    Store, StoredMediaMarker,
    test_support::{FixtureFile, publish_fixture_file_units},
};

fn fixture() -> (tempfile::TempDir, Store, domain::FileCoverage) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root = temp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
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
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let path = root.join("S01E01E02.mkv");
    std::fs::write(&path, b"fixture").unwrap();
    let file = publish_fixture_file_units(
        &store,
        &FixtureFile {
            media_id,
            path: path.display().to_string(),
            ..Default::default()
        },
        &[UnitKey::new(1, 1), UnitKey::new(1, 2)],
    )
    .unwrap();
    let coverage = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    (temp, store, coverage)
}

#[test]
fn checked_merge_preserves_second_unit_without_polluting_physical_chapters() {
    let (_temp, store, coverage) = fixture();
    let members =
        store::observe_sources(&store.source_snapshots(&[coverage.file.file_id]).unwrap());
    let marker = StoredMediaMarker {
        media_id: coverage.file.media_id,
        season: 1,
        episode: 2,
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "review".into(),
        locked: false,
        updated_at: 0,
    };
    store
        .merge_marker_results_checked(
            std::slice::from_ref(&coverage),
            &members,
            &[marker.clone()],
            false,
        )
        .unwrap();
    assert!(
        store
            .get_media_marker(marker.media_id, Some(1), Some(2))
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_media_marker(marker.media_id, Some(1), Some(1))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_cached_chapters(&coverage.file.file_id.to_string())
            .unwrap()
            .is_none()
    );
}

#[test]
fn television_whole_unit_does_not_claim_first_episode_marker() {
    let (_temp, store, coverage) = fixture();
    let scope = store::library_write::WriteScope {
        library_id: coverage.file.library_id,
        config_revision: store
            .library_config_revision(&coverage.file.library_id.to_string())
            .unwrap(),
    };
    store
        .publish_files(
            &scope,
            &[store::library_write::FilePublication {
                file: &coverage.file,
                units: &[UnitKey::WHOLE],
                expected_version: Some(&coverage.file.version),
                nfo_observation: None,
                probe_required: false,
            }],
            0,
        )
        .unwrap();
    let coverage = store
        .file_coverage_by_id(coverage.file.file_id)
        .unwrap()
        .unwrap();
    let members =
        store::observe_sources(&store.source_snapshots(&[coverage.file.file_id]).unwrap());
    let marker = StoredMediaMarker {
        media_id: coverage.file.media_id,
        season: 1,
        episode: 1,
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "review".into(),
        locked: false,
        updated_at: 0,
    };
    let updates = store
        .merge_marker_results_checked(&[coverage], &members, &[marker.clone()], false)
        .unwrap();
    assert!(updates.is_empty());
    assert!(
        store
            .get_media_marker(marker.media_id, Some(1), Some(1))
            .unwrap()
            .is_none()
    );
}

#[test]
fn television_whole_refresh_preserves_first_episode_marker() {
    let (_temp, store, coverage) = fixture();
    let file = coverage.file;
    let scope = store::library_write::WriteScope {
        library_id: file.library_id,
        config_revision: store
            .library_config_revision(&file.library_id.to_string())
            .unwrap(),
    };
    store
        .publish_files(
            &scope,
            &[store::library_write::FilePublication {
                file: &file,
                units: &[UnitKey::WHOLE],
                expected_version: Some(&file.version),
                nfo_observation: None,
                probe_required: false,
            }],
            0,
        )
        .unwrap();
    let marker = StoredMediaMarker {
        media_id: file.media_id,
        season: 1,
        episode: 1,
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "existing".into(),
        locked: false,
        updated_at: 0,
    };
    store.put_media_marker(&marker).unwrap();
    let id = file.file_id.to_string();
    store
        .create_probe_job(
            "whole-job",
            "marker_refresh",
            &file.media_id.to_string(),
            Some(1),
            "whole-scope",
            &[store::ProbeJobUnitSpec {
                ledger_id: &id,
                kind: "tv",
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: true,
            }],
        )
        .unwrap();
    store.start_probe_unit("whole-job", &id).unwrap();
    store
        .finish_probe_unit("whole-job", &id, true, None)
        .unwrap();
    let members = store::observe_sources(&store.source_snapshots(&[file.file_id]).unwrap());
    store
        .complete_marker_refresh_checked(
            "whole-job",
            &[store::MarkerResultReplacement {
                media_id: file.media_id,
                season: 1,
                markers: vec![],
                chapter_updates: vec![(id, vec![])],
            }],
            &members,
        )
        .unwrap();
    assert_eq!(
        store
            .get_media_marker(file.media_id, Some(1), Some(1))
            .unwrap()
            .unwrap()
            .source,
        "existing"
    );
}

#[test]
fn unsupported_multiunit_refresh_preserves_existing_markers() {
    let (_temp, store, coverage) = fixture();
    let marker = StoredMediaMarker {
        media_id: coverage.file.media_id,
        season: 1,
        episode: 2,
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "existing".into(),
        locked: false,
        updated_at: 0,
    };
    store.put_media_marker(&marker).unwrap();
    let result = store.replace_marker_results(
        marker.media_id,
        1,
        &[],
        &[(coverage.file.file_id.to_string(), vec![])],
    );
    assert!(
        result.is_err(),
        "multiunit automatic refresh needs a time mapping"
    );
    assert!(
        store
            .get_media_marker(marker.media_id, Some(1), Some(2))
            .unwrap()
            .is_some()
    );
}

#[test]
fn checked_merge_rejects_coverage_changed_without_source_change() {
    let (_temp, store, coverage) = fixture();
    let members =
        store::observe_sources(&store.source_snapshots(&[coverage.file.file_id]).unwrap());
    publish_fixture_file_units(
        &store,
        &FixtureFile {
            file_id: Some(coverage.file.file_id),
            media_id: coverage.file.media_id,
            path: coverage.file.path.display().to_string(),
            ..Default::default()
        },
        &[UnitKey::new(1, 1)],
    )
    .unwrap();
    let current = store
        .file_coverage_by_id(coverage.file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.file.version, coverage.file.version);
    let marker = StoredMediaMarker {
        media_id: coverage.file.media_id,
        season: 1,
        episode: 2,
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "review".into(),
        locked: false,
        updated_at: 0,
    };
    assert!(
        store
            .merge_marker_results_checked(&[coverage], &members, &[marker.clone()], false)
            .is_err()
    );
    assert!(
        store
            .get_media_marker(marker.media_id, Some(1), Some(2))
            .unwrap()
            .is_none()
    );
}

#[test]
fn singleton_marker_merge_preserves_physical_chapter_cache() {
    let (_temp, store, coverage) = fixture();
    publish_fixture_file_units(
        &store,
        &FixtureFile {
            file_id: Some(coverage.file.file_id),
            media_id: coverage.file.media_id,
            path: coverage.file.path.display().to_string(),
            ..Default::default()
        },
        &[UnitKey::new(1, 1)],
    )
    .unwrap();
    let current = store
        .file_coverage_by_id(coverage.file.file_id)
        .unwrap()
        .unwrap();
    let physical = vec![library::ChapterMarker {
        start_ms: 120_000,
        end_ms: 180_000,
        title: Some("Physical".into()),
        marker_type: None,
        synthetic: false,
    }];
    let id = current.file.file_id.to_string();
    store.put_cached_chapters(&id, &physical).unwrap();
    let members = store::observe_sources(&store.source_snapshots(&[current.file.file_id]).unwrap());
    store
        .merge_marker_results_checked(
            &[current.clone()],
            &members,
            &[StoredMediaMarker {
                media_id: current.file.media_id,
                season: 1,
                episode: 1,
                intro_start_ms: Some(10_000),
                intro_end_ms: Some(70_000),
                outro_start_ms: None,
                outro_end_ms: None,
                source: "review".into(),
                locked: false,
                updated_at: 0,
            }],
            false,
        )
        .unwrap();
    assert_eq!(store.get_cached_chapters(&id).unwrap().unwrap(), physical);
}
