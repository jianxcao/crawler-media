use super::*;
use api::probe_manager::ProbeUnit;

fn fixture() -> (
    tempfile::TempDir,
    Arc<Mutex<Store>>,
    Vec<(domain::FileCoverage, String)>,
) {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(temp.path().join("data")).unwrap()));
    let root = temp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let media_id = MediaId::new();
    let mut rows = Vec::new();
    {
        let store = store.lock();
        store
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Tv,
                title: "Window".into(),
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
        store
            .set_library_intro_settings(&library.id, true, true)
            .unwrap();
        for episode in 1..=18 {
            let path = root.join(format!("episode-{episode}.mkv"));
            std::fs::write(&path, b"video").unwrap();
            let file = publish_fixture_file(
                &store,
                &FixtureFile {
                    media_id,
                    path: path.to_string_lossy().into_owned(),
                    season: Some(1),
                    episode: Some(episode),
                    quality_source: QualitySource::Release,
                    confidence: Confidence::High,
                    ..Default::default()
                },
            )
            .unwrap();
            let source = api::fingerprint_job::current_source_version(&path);
            store
                .put_file_meta_versioned(
                    &file.file_id.to_string(),
                    &library::Tracks {
                        video: Some(library::VideoTrack::default()),
                        ..Default::default()
                    },
                    Some(&source),
                    Some(1_200_000),
                )
                .unwrap();
            rows.push((
                store.file_coverage_by_id(file.file_id).unwrap().unwrap(),
                source,
            ));
        }
    }
    (temp, store, rows)
}

#[tokio::test]
async fn full_fingerprint_window_preserves_completed_metadata() {
    let (_temp, store, rows) = fixture();
    let gate = Gate::new();
    let capture = Arc::new(GatedCapture::new(gate.clone()));
    let manager =
        Arc::new(ProbeManager::with_engines(store.clone(), capture.clone(), capture).unwrap());
    let unit = |row: domain::FileCoverage| ProbeUnit {
        row,
        kind: MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };
    for (row, source) in rows.iter().take(16) {
        assert!(
            manager
                .enqueue_direct_fingerprint(unit(row.clone()), Some(1_200_000), source.clone(), 60)
                .is_some()
        );
    }
    assert!(manager.try_start_workers());
    let entered = tokio::time::timeout(Duration::from_secs(5), async {
        while gate.entered() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if entered.is_err() {
        gate.release();
    }
    entered.expect("fingerprint worker must enter the capture gate");
    let (row, source) = &rows[16];
    assert!(
        manager
            .enqueue_direct_fingerprint(unit(row.clone()), Some(1_200_000), source.clone(), 60)
            .is_some()
    );
    assert_eq!(
        manager.queued_ledger_count(),
        17,
        "one active capture and sixteen queued fingerprints fill the window"
    );
    let target = &rows[17].0;
    assert!(manager.enqueue(unit(target.clone())));
    let metadata = store
        .lock()
        .active_probe_unit_for_ledger(&target.file.file_id.to_string())
        .unwrap()
        .unwrap()
        .job_id;
    let handed_off = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let ready = {
                let store = store.lock();
                let metadata_done =
                    store.get_probe_job(&metadata).unwrap().unwrap().status == "succeeded";
                let fingerprint = store
                    .active_probe_unit_for_ledger(&target.file.file_id.to_string())
                    .unwrap();
                metadata_done
                    && fingerprint.is_some_and(|item| {
                        item.stage == store::ProbeDispatchStage::Fingerprint
                            && item.status == "queued"
                    })
            };
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    gate.release();
    if handed_off.is_err() {
        let store = store.lock();
        panic!(
            "metadata handoff timed out: job={:?}, unit={:?}",
            store.get_probe_job(&metadata).unwrap(),
            store
                .active_probe_unit_for_ledger(&target.file.file_id.to_string())
                .unwrap()
        );
    }
    manager.shutdown_workers().await.unwrap();
}
