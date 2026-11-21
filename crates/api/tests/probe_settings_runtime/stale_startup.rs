use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_startup_does_not_publish_a_stale_pause_snapshot() {
    let _serial = PROCESS_GATE.lock().await;
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path());
    let handle = tokio::runtime::Handle::current();
    let (loaded_tx, loaded_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker_state = state.clone();
    let worker = std::thread::spawn(move || {
        let _entered = handle.enter();
        worker_state
            .store()
            .with_read_snapshot(&[store::Db::App], Duration::from_secs(5), || {
                assert_eq!(
                    worker_state
                        .store()
                        .get_setting("probe.remote_reads")
                        .unwrap(),
                    None
                );
                loaded_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                assert!(worker_state.probe.try_start_workers());
            })
            .unwrap();
    });
    loaded_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        remote_reads_request(&api::router(state.clone()), "PUT", json!({"paused": true})).await,
        json!({"paused": true})
    );
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    assert!(marker::media_read::remote_reads_paused());
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_does_not_clobber_concurrent_resume_remote_reads() {
    let _serial = PROCESS_GATE.lock().await;
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_path_buf();
    let media_dir = root.path().join("remote-resume");
    std::fs::create_dir_all(&media_dir).unwrap();

    // Persist `paused: true` and queue a `.strm` probe before workers start.
    let (waiting, mut wait_rx) = tokio::sync::mpsc::unbounded_channel();
    let (reads, mut read_rx) = tokio::sync::mpsc::unbounded_channel();
    let unstarted = std::thread::spawn(move || state(&root_path))
        .join()
        .unwrap()
        .with_probe_engines(
            Arc::new(marker::ChromaprintEngine),
            Arc::new(marker::ChromaprintEngine),
            Arc::new(GatedTracks {
                waiting,
                reads: HeldTracks(reads),
            }),
        )
        .unwrap();
    let app = api::router(unstarted.clone());
    assert_eq!(
        remote_reads_request(&app, "PUT", json!({"paused": true})).await,
        json!({"paused": true})
    );
    let store = unstarted.store();
    store
        .create_library(
            domain::MediaKind::Movie,
            "RemoteResume",
            &[media_dir.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let strm_path = media_dir.join("resume_me.strm");
    let queued = queue_movie_source(
        &unstarted,
        &store,
        &strm_path,
        0,
        "https://fixture.invalid/resume_me.mkv",
    );

    // Interleave `PUT {"paused": false}` between startup reading persisted `paused: true`
    // and completing worker startup.
    let handle = tokio::runtime::Handle::current();
    let (loaded_tx, loaded_rx) = std::sync::mpsc::channel();
    let (resumed_tx, resumed_rx) = std::sync::mpsc::channel();
    let worker_state = unstarted.clone();
    let starter = std::thread::spawn(move || {
        let _entered = handle.enter();
        assert!(worker_state.probe.try_start_workers_observed(|| {
            loaded_tx.send(()).unwrap();
            resumed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }));
    });
    loaded_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        remote_reads_request(&app, "PUT", json!({"paused": false})).await,
        json!({"paused": false})
    );
    resumed_tx.send(()).unwrap();
    starter.join().unwrap();

    // The queued STRM task must genuinely acquire its remote read permit and finish.
    let _ = tokio::time::timeout(Duration::from_secs(3), wait_rx.recv()).await;
    let (started_path, release) = tokio::time::timeout(Duration::from_secs(5), read_rx.recv())
        .await
        .expect("STRM task must acquire a remote read permit after concurrent resume")
        .unwrap();
    assert_eq!(started_path, queued.file.path);
    release.send(()).unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if store
            .active_probe_unit_for_ledger(&queued.file.file_id.to_string())
            .unwrap()
            .is_none()
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        store
            .file_coverage_by_id(queued.file.file_id)
            .unwrap()
            .unwrap()
            .file
            .duration_ms
            .is_some()
    );
    unstarted.probe.shutdown_workers().await.unwrap();
}
