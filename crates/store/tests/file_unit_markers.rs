use domain::{Media, MediaId, MediaKind, UnitKey};
use store::{
    Store, StoredFileMarker,
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
fn marker_cannot_be_read_through_another_file_identity() {
    let (_temp, store, coverage) = fixture();
    let members =
        store::observe_sources(&store.source_snapshots(&[coverage.file.file_id]).unwrap());
    let marker = store::StoredFileMarker {
        file_id: coverage.file.file_id,
        version: coverage.file.version.clone(),
        unit: UnitKey::new(1, 2),
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "fingerprint".into(),
        locked: false,
        updated_at: 0,
    };
    store.put_file_marker_checked(&marker, &members).unwrap();
    assert!(
        store
            .get_file_marker(marker.file_id, marker.unit)
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_file_marker(domain::LedgerId::new(), marker.unit)
            .unwrap()
            .is_none()
    );
}

fn exact_marker(file: &domain::LibraryFile, unit: UnitKey, start: i64) -> StoredFileMarker {
    StoredFileMarker {
        file_id: file.file_id,
        version: file.version.clone(),
        unit,
        intro_start_ms: Some(start),
        intro_end_ms: Some(start + 60_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "fingerprint".into(),
        locked: false,
        updated_at: 0,
    }
}

#[test]
fn two_cuts_of_one_episode_keep_independent_markers_and_hydration() {
    let (_temp, store, first) = fixture();
    let second_path = first.file.path.with_file_name("other-cut.mkv");
    std::fs::write(&second_path, b"other cut").unwrap();
    let second = publish_fixture_file_units(
        &store,
        &FixtureFile {
            media_id: first.file.media_id,
            path: second_path.display().to_string(),
            ..Default::default()
        },
        &first.units,
    )
    .unwrap();
    let unit = UnitKey::new(1, 2);
    for (file, start) in [(&first.file, 10_000), (&second, 40_000)] {
        let members = store::observe_sources(&store.source_snapshots(&[file.file_id]).unwrap());
        store
            .put_file_marker_checked(&exact_marker(file, unit, start), &members)
            .unwrap();
    }
    let facts = store
        .page_hydration(&[first.file.file_id, second.file_id])
        .unwrap();
    assert_eq!(
        facts[&first.file.file_id].markers[&(1, 2)].intro_start_ms,
        Some(10_000)
    );
    assert_eq!(
        facts[&second.file_id].markers[&(1, 2)].intro_start_ms,
        Some(40_000)
    );
    assert_eq!(
        store
            .get_file_marker(first.file.file_id, unit)
            .unwrap()
            .unwrap()
            .intro_start_ms,
        Some(10_000)
    );
    assert_eq!(
        store
            .get_file_marker(second.file_id, unit)
            .unwrap()
            .unwrap()
            .intro_start_ms,
        Some(40_000)
    );
}

#[test]
fn deleting_one_library_file_keeps_another_librarys_locked_marker() {
    let (temp, store, first) = fixture();
    let root = temp.path().join("another-library");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Another",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let path = root.join("S01E02.mkv");
    std::fs::write(&path, b"other library").unwrap();
    let unit = UnitKey::new(1, 2);
    let second = publish_fixture_file_units(
        &store,
        &FixtureFile {
            media_id: first.file.media_id,
            path: path.display().to_string(),
            ..Default::default()
        },
        &[unit],
    )
    .unwrap();
    for file in [&first.file, &second] {
        let mut marker = exact_marker(file, unit, 10_000);
        marker.locked = true;
        let members = store::observe_sources(&store.source_snapshots(&[file.file_id]).unwrap());
        store.put_file_marker_checked(&marker, &members).unwrap();
    }
    store
        .delete_files(first.file.library_id, &[first.file.file_id])
        .unwrap();
    assert!(
        store
            .get_file_marker(first.file.file_id, unit)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_file_marker(second.file_id, unit)
            .unwrap()
            .unwrap()
            .locked
    );
}

#[test]
fn idempotent_registration_keeps_marker_but_source_change_invalidates_even_locked_marker() {
    let (_temp, store, coverage) = fixture();
    let unit = UnitKey::new(1, 2);
    let mut old = exact_marker(&coverage.file, unit, 10_000);
    old.locked = true;
    let members = store::observe_sources(&store.source_snapshots(&[old.file_id]).unwrap());
    store.put_file_marker_checked(&old, &members).unwrap();
    let scope = store::library_write::WriteScope {
        library_id: coverage.file.library_id,
        config_revision: store
            .library_config_revision(&coverage.file.library_id.to_string())
            .unwrap(),
    };
    let publish = |file: &domain::LibraryFile| {
        store.publish_files(
            &scope,
            &[store::library_write::FilePublication {
                file,
                units: &coverage.units,
                expected_version: Some(&coverage.file.version),
                nfo_observation: None,
                probe_required: false,
            }],
            0,
        )
    };
    publish(&coverage.file).unwrap();
    assert!(
        store
            .get_file_marker(old.file_id, unit)
            .unwrap()
            .unwrap()
            .locked
    );
    let mut replacement = coverage.file.clone();
    replacement.version.source_version = "different-cut".into();
    publish(&replacement).unwrap();
    assert!(store.get_file_marker(old.file_id, unit).unwrap().is_none());
    assert!(
        store.page_hydration(&[old.file_id]).unwrap()[&old.file_id]
            .markers
            .is_empty()
    );
    assert!(store.put_file_marker_checked(&old, &members).is_err());
    let current = store.file_coverage_by_id(old.file_id).unwrap().unwrap();
    let members = store::observe_sources(&store.source_snapshots(&[old.file_id]).unwrap());
    store
        .put_file_marker_checked(&exact_marker(&current.file, unit, 40_000), &members)
        .unwrap();
    let current = store.get_file_marker(old.file_id, unit).unwrap().unwrap();
    assert_eq!(current.intro_start_ms, Some(40_000));
    assert!(!current.locked);
}

#[test]
fn empty_result_rejects_changed_units_without_erasing_the_new_units_marker() {
    let (_temp, store, coverage) = fixture();
    let scope = store::library_write::WriteScope {
        library_id: coverage.file.library_id,
        config_revision: store
            .library_config_revision(&coverage.file.library_id.to_string())
            .unwrap(),
    };
    let publish = |unit| {
        store.publish_files(
            &scope,
            &[store::library_write::FilePublication {
                file: &coverage.file,
                units: &[unit],
                expected_version: Some(&coverage.file.version),
                nfo_observation: None,
                probe_required: false,
            }],
            0,
        )
    };
    publish(UnitKey::new(1, 1)).unwrap();
    let old = store
        .file_coverage_by_id(coverage.file.file_id)
        .unwrap()
        .unwrap();
    let members = store::observe_sources(&store.source_snapshots(&[old.file.file_id]).unwrap());
    let replacement = store::MarkerResultReplacement {
        media_id: old.file.media_id,
        season: 1,
        markers: vec![],
        targets: vec![old.clone()],
        chapter_updates: vec![(old.file.file_id.to_string(), vec![])],
    };
    publish(UnitKey::new(1, 2)).unwrap();
    let current = store
        .file_coverage_by_id(old.file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        current.file.version, old.file.version,
        "coverage changed without changing the source"
    );
    let marker = exact_marker(&current.file, UnitKey::new(1, 2), 40_000);
    store.put_file_marker_checked(&marker, &members).unwrap();
    assert!(
        store
            .replace_marker_results_batch_checked(&[replacement], &members)
            .is_err()
    );
    assert_eq!(
        store
            .get_file_marker(marker.file_id, marker.unit)
            .unwrap()
            .unwrap()
            .intro_start_ms,
        Some(40_000)
    );
}

#[test]
fn checked_merge_preserves_second_unit_without_polluting_physical_chapters() {
    let (_temp, store, coverage) = fixture();
    let members =
        store::observe_sources(&store.source_snapshots(&[coverage.file.file_id]).unwrap());
    let marker = StoredFileMarker {
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "review".into(),
        locked: false,
        updated_at: 0,
        file_id: coverage.file.file_id,
        version: coverage.file.version.clone(),
        unit: UnitKey::new(1, 2),
    };
    store
        .merge_marker_results_checked(
            std::slice::from_ref(&coverage),
            &members,
            std::slice::from_ref(&marker),
            false,
        )
        .unwrap();
    assert!(
        store
            .get_file_marker(marker.file_id, UnitKey::new(1, 2))
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_file_marker(marker.file_id, UnitKey::new(1, 1))
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
    let marker = StoredFileMarker {
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "review".into(),
        locked: false,
        updated_at: 0,
        file_id: coverage.file.file_id,
        version: coverage.file.version.clone(),
        unit: UnitKey::new(1, 1),
    };
    let rejected = store
        .merge_marker_results_checked(&[coverage], &members, std::slice::from_ref(&marker), false)
        .is_err();
    assert!(rejected);
    assert!(
        store
            .get_file_marker(marker.file_id, UnitKey::new(1, 1))
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
    let episode_path = file.path.with_file_name("S01E01.mkv");
    std::fs::write(&episode_path, b"episode cut").unwrap();
    let episode_file = publish_fixture_file_units(
        &store,
        &FixtureFile {
            media_id: file.media_id,
            path: episode_path.display().to_string(),
            ..Default::default()
        },
        &[UnitKey::new(1, 1)],
    )
    .unwrap();
    let marker = StoredFileMarker {
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "existing".into(),
        locked: false,
        updated_at: 0,
        file_id: episode_file.file_id,
        version: episode_file.version.clone(),
        unit: UnitKey::new(1, 1),
    };
    store.put_fixture_marker(&marker).unwrap();
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
            &[store::test_support::fixture_marker_replacement(
                &store,
                file.media_id,
                1,
                vec![],
                vec![(id, vec![])],
            )],
            &members,
        )
        .unwrap();
    assert_eq!(
        store
            .get_fixture_marker(file.media_id, Some(1), Some(1))
            .unwrap()
            .unwrap()
            .source,
        "existing"
    );
}

#[test]
fn unsupported_multiunit_refresh_preserves_existing_markers() {
    let (_temp, store, coverage) = fixture();
    let marker = StoredFileMarker {
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "existing".into(),
        locked: false,
        updated_at: 0,
        file_id: coverage.file.file_id,
        version: coverage.file.version.clone(),
        unit: UnitKey::new(1, 2),
    };
    store.put_fixture_marker(&marker).unwrap();
    let result = store.replace_marker_results(
        coverage.file.media_id,
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
            .get_file_marker(marker.file_id, UnitKey::new(1, 2))
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
    let marker = StoredFileMarker {
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "review".into(),
        locked: false,
        updated_at: 0,
        file_id: coverage.file.file_id,
        version: coverage.file.version.clone(),
        unit: UnitKey::new(1, 2),
    };
    assert!(
        store
            .merge_marker_results_checked(
                &[coverage],
                &members,
                std::slice::from_ref(&marker),
                false
            )
            .is_err()
    );
    assert!(
        store
            .get_file_marker(marker.file_id, UnitKey::new(1, 2))
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
            std::slice::from_ref(&current),
            &members,
            &[StoredFileMarker {
                intro_start_ms: Some(10_000),
                intro_end_ms: Some(70_000),
                outro_start_ms: None,
                outro_end_ms: None,
                source: "review".into(),
                locked: false,
                updated_at: 0,
                ..store::test_support::fixture_marker(
                    &store,
                    current.file.media_id,
                    domain::UnitKey::new(1, 1),
                )
            }],
            false,
        )
        .unwrap();
    assert_eq!(store.get_cached_chapters(&id).unwrap().unwrap(), physical);
}
