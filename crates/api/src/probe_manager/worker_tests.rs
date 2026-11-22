use super::*;

fn add_running_job(store: &store::Store, job: &str, file: &str) {
    assert!(
        store
            .create_probe_job(
                job,
                "media_probe",
                "media",
                None,
                job,
                &[store::ProbeJobUnitSpec {
                    ledger_id: file,
                    kind: "movie",
                    force_fingerprint: false,
                    reuse_fingerprint_cache: false,
                    overwrite_markers: false,
                    reuse_media_info_cache: false,
                }]
            )
            .unwrap()
    );
    assert!(
        store
            .start_probe_unit(job, file, store::ProbeDispatchStage::Metadata)
            .unwrap()
    );
}

#[tokio::test]
async fn driver_step_waits_for_blocking_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(store::Store::open(directory.path()).unwrap());
    let manager = Arc::new(ProbeManager::new(store).unwrap());
    let permits: Vec<_> = (0..12)
        .map(|_| manager.dispatch_work.try_reserve().unwrap())
        .collect();
    let task_manager = manager.clone();
    let task = tokio::spawn(async move { step(&task_manager, |_| 42).await });
    tokio::task::yield_now().await;
    drop(permits);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        42
    );
}

#[tokio::test]
async fn interrupted_unit_retries_after_store_contention_without_resetting_other_work() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(store::Store::open(directory.path()).unwrap());
    let manager = Arc::new(ProbeManager::new(store.clone()).unwrap());
    add_running_job(&store.as_ref(), "first", "file-1");
    add_running_job(&store.as_ref(), "second", "file-2");
    manager.seen.lock().insert("file-1".into());
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let held_store = store.clone();
    let holder = std::thread::spawn(move || {
        held_store
            .with_writer(store::Db::Library, |_| {
                held_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .unwrap();
    });
    held_rx.recv().unwrap();
    let task_manager = manager.clone();
    let task = tokio::spawn(async move {
        retry_dispatch(
            &task_manager,
            &store::ProbeDispatchId {
                job_id: "first".into(),
                ledger_id: "file-1".into(),
                stage: store::ProbeDispatchStage::Metadata,
            },
            true,
        )
        .await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(!task.is_finished());
    assert!(manager.seen.lock().contains("file-1"));
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    let store = store.as_ref();
    assert_eq!(store.probe_job_units("first").unwrap()[0].status, "queued");
    assert_eq!(
        store.probe_job_units("second").unwrap()[0].status,
        "running"
    );
    assert!(!manager.seen.lock().contains("file-1"));
}

#[tokio::test]
async fn old_cleanup_does_not_release_new_job_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(store::Store::open(directory.path()).unwrap());
    let manager = Arc::new(ProbeManager::new(store.clone()).unwrap());
    add_running_job(&store.as_ref(), "old", "file");
    store.as_ref().cancel_probe_job("old", "cancelled").unwrap();
    add_running_job(&store.as_ref(), "new", "file");
    manager.seen.lock().insert("file".into());
    retry_dispatch(
        &manager,
        &store::ProbeDispatchId {
            job_id: "old".into(),
            ledger_id: "file".into(),
            stage: store::ProbeDispatchStage::Metadata,
        },
        true,
    )
    .await;
    assert!(manager.seen.lock().contains("file"));
    assert_eq!(
        store.as_ref().probe_job_units("new").unwrap()[0].status,
        "running"
    );
}

struct BlockedFirstTracks {
    entered_tx: parking_lot::Mutex<Option<std::sync::mpsc::Sender<()>>>,
    release_rx: parking_lot::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl super::super::TracksProbe for BlockedFirstTracks {
    fn probe(
        &self,
        path: &std::path::Path,
        _control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "movie-000.mkv" {
            if let Some(tx) = self.entered_tx.lock().take() {
                let _ = tx.send(());
            }
            if let Some(rx) = self.release_rx.lock().take() {
                let _ = rx.recv();
            }
        } else if name == "movie-065.mkv" {
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
        Ok((library::Tracks::default(), Some(60_000)))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metadata_idle_waits_for_overflow_window_refill_before_notifying() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let store = Arc::new(store::Store::open(tmp.path().join("data")).unwrap());
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let manager = Arc::new(
        ProbeManager::new(store.clone())
            .unwrap()
            .with_tracks_probe(Arc::new(BlockedFirstTracks {
                entered_tx: parking_lot::Mutex::new(Some(entered_tx)),
                release_rx: parking_lot::Mutex::new(Some(release_rx)),
            })),
    );
    // Start workers first on empty DB, mirroring production startup.
    assert!(manager.try_start_workers());
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Overflow Movie".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.as_ref().insert_media(&media).unwrap();
    let _lib = store
        .as_ref()
        .create_library(
            domain::MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let total = super::super::dispatch::METADATA_WINDOW + 2;
    let mut ledger_ids = Vec::with_capacity(total);
    for idx in 0..total {
        let path = root.join(format!("movie-{idx:03}.mkv"));
        std::fs::write(&path, b"video").unwrap();
        let cov =
            super::super::test_support::coverage(media.id, &path.to_string_lossy(), None, None);
        let published = super::super::test_support::publish(store.as_ref(), &cov);
        ledger_ids.push(published.file.file_id.to_string());
    }
    let specs: Vec<_> = ledger_ids
        .iter()
        .map(|id| store::ProbeJobUnitSpec {
            ledger_id: id,
            kind: "movie",
            force_fingerprint: false,
            reuse_fingerprint_cache: false,
            overwrite_markers: false,
            reuse_media_info_cache: false,
        })
        .collect();
    assert!(
        store
            .as_ref()
            .create_probe_job(
                "overflow-job",
                "media_probe",
                &media.id.to_string(),
                None,
                "overflow-scope",
                &specs,
            )
            .unwrap()
    );

    // Fill 64 into channel; worker pops movie-000 and blocks. Then fill once more so
    // movie-064 takes slot 64 while movie-065 stays queued in SQLite backlog.
    manager.fill_dispatch_windows();
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("worker must pop movie-000");
    manager.fill_dispatch_windows();

    let gate = manager.priority_gate();
    let waiter = tokio::spawn(async move {
        gate.wait_before_capture().await;
    });
    tokio::task::yield_now().await;

    release_tx.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), waiter)
        .await
        .expect("metadata queue must become idle after draining overflow")
        .unwrap();

    let units = store.as_ref().probe_job_units("overflow-job").unwrap();
    let pending: Vec<_> = units
        .iter()
        .filter(|u| matches!(u.status.as_str(), "queued" | "running"))
        .map(|u| (&u.ledger_id, &u.status))
        .collect();
    assert!(
        pending.is_empty(),
        "metadata_idle fired before refilling and completing overflowed units: remaining={pending:?}"
    );
    let _ = manager.shutdown_workers().await;
}

struct AlwaysRetryTracks {
    calls: std::sync::atomic::AtomicUsize,
}

impl super::super::TracksProbe for AlwaysRetryTracks {
    fn probe(
        &self,
        _: &std::path::Path,
        _: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(library::LibraryError::ReadWaitStopped)
    }
}

struct AlwaysRetryCapture {
    calls: std::sync::atomic::AtomicUsize,
}

impl marker::FingerprintEngine for AlwaysRetryCapture {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        Ok(vec![1, 2, 3])
    }
    fn find_common_segment(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<marker::CommonSegment> {
        None
    }
}

impl marker::FingerprintCaptureEngine for AlwaysRetryCapture {
    fn capture_window(
        &self,
        _: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(marker::CaptureFailure {
            kind: marker::CaptureFailureKind::Cancelled,
            message: marker::media_read::READ_WAIT_STOPPED.into(),
            metrics: marker::CaptureMetrics::default(),
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_dispatch_sleeps_100ms_and_aborts_promptly_on_shutdown() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("shows");
    std::fs::create_dir_all(&root).unwrap();
    let store = Arc::new(store::Store::open(tmp.path().join("data")).unwrap());
    let lib = store
        .create_library(
            domain::MediaKind::Tv,
            "Shows",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&lib.id, true, true)
        .unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "Retry Show".into(),
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
    let meta_path = root.join("Show.S01E01.mkv");
    let fp_path = root.join("Show.S01E02.mkv");
    std::fs::write(&meta_path, b"meta").unwrap();
    std::fs::write(&fp_path, b"fp").unwrap();
    let meta_row = super::super::test_support::publish(
        &store,
        &super::super::test_support::coverage(
            media.id,
            meta_path.to_str().unwrap(),
            Some(1),
            Some(1),
        ),
    );
    let fp_row = super::super::test_support::publish(
        &store,
        &super::super::test_support::coverage(
            media.id,
            fp_path.to_str().unwrap(),
            Some(1),
            Some(2),
        ),
    );
    let tracks = Arc::new(AlwaysRetryTracks {
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let capture = Arc::new(AlwaysRetryCapture {
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let manager = Arc::new(
        ProbeManager::with_engines(store.clone(), capture.clone(), capture.clone())
            .unwrap()
            .with_tracks_probe(tracks.clone()),
    );
    assert!(manager.try_start_workers());
    assert!(manager.enqueue(ProbeUnit {
        row: meta_row,
        kind: domain::MediaKind::Tv,
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: false,
        marker_refresh_id: None,
        job_id: None,
    }));
    manager.enqueue_direct_fingerprint(
        ProbeUnit {
            row: fp_row.clone(),
            kind: domain::MediaKind::Tv,
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: false,
            reuse_media_info_cache: false,
            marker_refresh_id: None,
            job_id: None,
        },
        Some(120_000),
        fp_row.file.version.source_version.clone(),
        180,
    );
    tokio::time::sleep(std::time::Duration::from_millis(260)).await;
    let meta_calls = tracks.calls.load(std::sync::atomic::Ordering::SeqCst);
    let fp_calls = capture.calls.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        (1..=4).contains(&meta_calls),
        "metadata retry must pace at ~100ms, got {meta_calls} calls in 260ms"
    );
    assert!(
        (1..=4).contains(&fp_calls),
        "fingerprint retry must pace at ~100ms, got {fp_calls} calls in 260ms"
    );
    let shutdown_started = std::time::Instant::now();
    manager.shutdown_workers().await.unwrap();
    assert!(
        shutdown_started.elapsed() < std::time::Duration::from_millis(90),
        "shutdown while retry_dispatch is sleeping must abort promptly, took {:?}",
        shutdown_started.elapsed()
    );
}
