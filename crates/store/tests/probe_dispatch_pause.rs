use std::path::Path;

use domain::{Media, MediaId, MediaKind};
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

fn enqueue_source(store: &Store, media: MediaId, path: &Path, job_id: &str) -> String {
    std::fs::write(path, "fixture").unwrap();
    let file = publish_fixture_file(
        store,
        &FixtureFile {
            media_id: media,
            path: path.display().to_string(),
            season: Some(1),
            episode: Some(
                if path
                    .extension()
                    .is_some_and(|extension| extension == "strm")
                {
                    1
                } else {
                    2
                },
            ),
            ..Default::default()
        },
    )
    .unwrap();
    let ledger_id = file.file_id.to_string();
    let created = store
        .create_probe_job(
            job_id,
            "fingerprint_probe",
            &media.to_string(),
            Some(1),
            job_id,
            &[store::ProbeJobUnitSpec {
                ledger_id: &ledger_id,
                kind: "tv",
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: false,
                reuse_media_info_cache: true,
            }],
        )
        .unwrap();
    assert!(
        created,
        "optional work should be enabled by the Library settings"
    );
    ledger_id
}

#[test]
fn paused_fingerprint_window_selects_local_sources_after_remote_backlog() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root = temp.path().join("episodes");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            "Episodes",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&library.id, true, true)
        .unwrap();
    let media = Media {
        id: MediaId::new(),
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
    };
    store.insert_media(&media).unwrap();

    let mut remote_ids = Vec::new();
    for index in 0..24 {
        remote_ids.push(enqueue_source(
            &store,
            media.id,
            &root.join(format!("Episode{index:02}.strm")),
            &format!("remote-{index:02}"),
        ));
    }
    let local_id = enqueue_source(&store, media.id, &root.join("Episode-local.mkv"), "local");

    let pending = store
        .pending_local_probe_dispatch_ids(store::ProbeDispatchStage::Fingerprint, 16)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].ledger_id, local_id);
    let remote_pending = store
        .pending_probe_dispatch_ids(store::ProbeDispatchStage::Fingerprint, 32)
        .unwrap();
    assert!(
        remote_ids
            .iter()
            .all(|id| remote_pending.iter().any(|row| &row.ledger_id == id))
    );
}
