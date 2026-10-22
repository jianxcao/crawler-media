//! Fixture publication test: the facts `test_support::publish_fixture_file`
//! publishes for one path.
//!
//! The former legacy-parity case (the removed legacy write entry point vs
//! `publish_fixture_file`) is gone with that entry point: with no legacy writer
//! left there is no equivalence proposition to verify.

use domain::{Confidence, Media, MediaId, MediaKind, QualitySource};
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

#[test]
fn fixture_publication_same_path_retains_file_id_and_advances_or_retains_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let root_fixture = tmp.path().join("fixture");
    std::fs::create_dir_all(&root_fixture).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Fixture Lib",
            &[root_fixture.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let media_fixture = MediaId::new();
    store
        .insert_media(&Media {
            id: media_fixture,
            kind: MediaKind::Movie,
            title: "Movie Fixture".into(),
            year: Some(2023),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("102".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let fixture_file = root_fixture.join("Matrix.mkv");
    std::fs::write(&fixture_file, b"content").unwrap();

    let spec = FixtureFile {
        media_id: media_fixture,
        path: fixture_file.to_str().unwrap().to_string(),
        season: None,
        episode: None,
        resolution: Some("1080p".into()),
        codec: Some("h264".into()),
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: Some(80),
        transfer_mode: None,
        source_path: None,
        file_id: None,
    };

    let first = publish_fixture_file(&store, &spec).unwrap();
    let second = publish_fixture_file(&store, &spec).unwrap();

    // Re-publishing the exact same path retains the same identity
    assert_eq!(second.file_id, first.file_id);
    assert_eq!(second.version.source_version, first.version.source_version);
}
