//! Delayed publication must retain its original context across Store connections.

use domain::{LibraryFile, Media, MediaId, MediaKind, UnitKey};
use store::Store;
use store::library_write::{FilePublication, WriteScope};
use store::test_support::{FixtureFile, publish_fixture_file};

struct Fixture {
    _tmp: tempfile::TempDir,
    writer: Store,
    other: Store,
    file: LibraryFile,
    scope: WriteScope,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let writer = Store::open(tmp.path().join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Context".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    writer.insert_media(&media).unwrap();
    writer
        .create_library(
            media.kind,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let path = root.join("Context.mkv");
    std::fs::write(&path, b"context video").unwrap();
    let file = publish_fixture_file(
        &writer,
        &FixtureFile {
            media_id: media.id,
            path: path.to_string_lossy().into_owned(),
            ..Default::default()
        },
    )
    .unwrap();
    let scope = WriteScope {
        library_id: file.library_id,
        config_revision: writer
            .library_config_revision(&file.library_id.to_string())
            .unwrap(),
    };
    let other = Store::open(tmp.path().join("data")).unwrap();
    Fixture {
        _tmp: tmp,
        writer,
        other,
        file,
        scope,
    }
}

fn delayed_publication(fixture: &Fixture) -> Result<(), store::StoreError> {
    let units = [UnitKey::WHOLE];
    fixture
        .writer
        .publish_file_checked_context(
            &fixture.scope,
            &FilePublication {
                file: &fixture.file,
                units: &units,
                expected_version: Some(&fixture.file.version),
                nfo_observation: None,
                probe_required: false,
            },
            Some(&fixture.file),
            1_000,
        )
        .map(|_| ())
}

#[test]
fn second_connection_rename_cannot_be_reversed_by_delayed_publication() {
    let fixture = fixture();
    let destination = fixture.file.path.with_file_name("Renamed.mkv");
    fixture
        .other
        .rename_file_within_root(
            fixture.file.file_id,
            &fixture.file.version,
            &fixture.file.path,
            &destination,
        )
        .unwrap();
    let after = fixture
        .other
        .file_coverage_by_id(fixture.file.file_id)
        .unwrap()
        .unwrap()
        .file;
    assert_eq!(
        after.version, fixture.file.version,
        "a rename keeps its source generation"
    );
    assert!(
        delayed_publication(&fixture).is_err(),
        "source CAS alone cannot guard a renamed locator"
    );
    assert_eq!(
        fixture
            .writer
            .file_coverage_by_id(fixture.file.file_id)
            .unwrap()
            .unwrap()
            .file
            .path,
        destination
    );
}

#[test]
fn second_connection_deletion_cannot_be_reversed_by_delayed_publication() {
    let fixture = fixture();
    fixture
        .other
        .delete_files(fixture.file.library_id, &[fixture.file.file_id])
        .unwrap();
    assert!(
        delayed_publication(&fixture).is_err(),
        "an observed existing file must not be inserted after deletion"
    );
    assert!(
        fixture
            .writer
            .file_coverage_by_id(fixture.file.file_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn second_connection_insertion_invalidates_an_absent_destination_snapshot() {
    let fixture = fixture();
    fixture
        .writer
        .delete_files(fixture.file.library_id, &[fixture.file.file_id])
        .unwrap();
    let mut delayed = fixture.file.clone();
    delayed.file_id = domain::LedgerId::new();
    fixture
        .other
        .publish_files(
            &fixture.scope,
            &[FilePublication {
                file: &fixture.file,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();
    let result = fixture.writer.publish_file_checked_context(
        &fixture.scope,
        &FilePublication {
            file: &delayed,
            units: &[UnitKey::WHOLE],
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        },
        None,
        1_100,
    );
    assert!(matches!(result, Err(store::StoreError::Protected(_))));
    assert_eq!(
        fixture
            .writer
            .file_coverage_by_path(delayed.path.to_str().unwrap())
            .unwrap()
            .unwrap()
            .file
            .file_id,
        fixture.file.file_id
    );
    assert!(
        fixture
            .writer
            .file_coverage_by_id(delayed.file_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn an_unchanged_absent_destination_snapshot_can_publish() {
    let fixture = fixture();
    fixture
        .other
        .delete_files(fixture.file.library_id, &[fixture.file.file_id])
        .unwrap();
    let result = fixture
        .writer
        .publish_file_checked_context(
            &fixture.scope,
            &FilePublication {
                file: &fixture.file,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            },
            None,
            1_000,
        )
        .unwrap();
    assert_eq!(result.inserted, 1);
    assert_eq!(
        fixture
            .writer
            .file_coverage_by_id(fixture.file.file_id)
            .unwrap()
            .unwrap()
            .file
            .version,
        fixture.file.version
    );
}
