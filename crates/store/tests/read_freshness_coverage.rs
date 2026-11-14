use domain::{Media, MediaId, MediaKind, UnitKey};
use store::Store;
use store::library_write::probe::{Observed, ProbePublication};
use store::test_support::{FixtureFile, publish_fixture_file_units};

#[test]
fn coverage_change_invalidates_read_fingerprint_without_changing_file_version() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root = temp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
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
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Episodes".into(),
            original_title: None,
            aliases: vec![],
            year: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let spec = FixtureFile {
        media_id,
        path: root.join("episodes.mkv").display().to_string(),
        ..Default::default()
    };
    let file = publish_fixture_file_units(&store, &spec, &[UnitKey::new(1, 1)]).unwrap();
    let before = store.read_fingerprint(&library.id, media_id).unwrap();
    let changed = FixtureFile {
        file_id: Some(file.file_id),
        ..spec
    };
    publish_fixture_file_units(&store, &changed, &[UnitKey::new(1, 1), UnitKey::new(1, 2)])
        .unwrap();
    assert_eq!(
        store.get_file_version(file.file_id).unwrap().unwrap(),
        file.version
    );
    assert_ne!(
        before,
        store.read_fingerprint(&library.id, media_id).unwrap()
    );
}

#[test]
fn confirming_sdr_invalidates_fingerprint_even_when_hdr_was_empty() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root = temp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
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
            kind: MediaKind::Movie,
            title: "Movie".into(),
            original_title: None,
            aliases: vec![],
            year: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let file = publish_fixture_file_units(
        &store,
        &FixtureFile {
            media_id,
            path: root.join("movie.mkv").display().to_string(),
            quality_source: domain::QualitySource::Probe,
            ..Default::default()
        },
        &[UnitKey::WHOLE],
    )
    .unwrap();
    let before = store.read_fingerprint(&library.id, media_id).unwrap();
    store
        .publish_probe_result(
            file.file_id,
            &ProbePublication {
                expected: &file.version,
                tracks_version: 0,
                resolution: None,
                codec: None,
                hdr: Observed::Value(None),
                duration_ms: None,
                details: None,
            },
            1,
        )
        .unwrap();
    assert_ne!(
        before,
        store.read_fingerprint(&library.id, media_id).unwrap()
    );
}
