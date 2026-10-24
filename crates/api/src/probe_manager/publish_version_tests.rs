use domain::{FileVersion, MediaId, MediaKind};
use library::Tracks;

use super::*;

#[test]
fn queued_version_rejects_a_result_probed_after_replacement() {
    let tmp = tempfile::tempdir().unwrap();
    let store = crate::Store::open(tmp.path().join("data")).unwrap();
    let media_id = MediaId::new();
    store
        .create_library(MediaKind::Movie, "Fixture", &[tmp.path().to_str().unwrap()], "everyone", true, &[])
        .unwrap();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: "Fixture".into(),
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
    let mut coverage = super::test_support::coverage(
        media_id,
        &tmp.path().join("movie.strm").display().to_string(),
        None,
        None,
    );
    std::fs::write(&coverage.file.path, b"v1").unwrap();
    super::test_support::publish_into(&store, &mut coverage);
    let queued = coverage.file.version.clone();
    let replaced = replace_content(&store, &coverage, &queued);
    let unit = super::ProbeUnit {
        row: coverage,
        kind: MediaKind::Movie,
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: false,
        marker_refresh_id: None,
        job_id: None,
    };
    let published = super::probe::publish_probe_tracks(
        &store,
        &unit,
        &queued.source_version,
        &Tracks::default(),
        Some(1_000),
    )
    .unwrap();
    assert!(!published, "a result for the queued version must not land on the replacement");
    let current = store.get_file_version(unit.row.file.file_id).unwrap().unwrap();
    assert_eq!(current, replaced);
    assert_ne!(current.source_version, queued.source_version);
}

fn replace_content(
    store: &crate::Store,
    coverage: &domain::FileCoverage,
    queued: &FileVersion,
) -> FileVersion {
    std::fs::write(&coverage.file.path, b"v2-different-content").unwrap();
    let observed = library::observe_source(&coverage.file.path).unwrap();
    let mut file = coverage.file.clone();
    file.version = FileVersion {
        revision: queued.revision + 1,
        source_version: observed.source_version,
    };
    let revision = store.library_config_revision(&file.library_id.to_string()).unwrap();
    store
        .publish_files(
            &store::library_write::WriteScope { library_id: file.library_id, config_revision: revision },
            &[store::library_write::FilePublication {
                file: &file,
                units: &coverage.units,
                expected_version: Some(queued),
                nfo_observation: None,
                probe_required: false,
            }],
            2_000,
        )
        .unwrap();
    store.get_file_version(file.file_id).unwrap().unwrap()
}
