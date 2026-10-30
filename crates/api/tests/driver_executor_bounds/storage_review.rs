use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metadata_publication_retries_after_store_is_busy() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let row = seed_probe_candidate_in(&store);
    let probe = GatedTracksProbe::new();
    let manager = Arc::new(
        api::probe_manager::ProbeManager::new(store.clone())
            .unwrap()
            .with_tracks_probe(probe.clone()),
    );
    assert!(manager.try_start_workers());
    let result = manager
        .request_probe(&row, ProbeRequestOrigin::Detail)
        .unwrap();
    let api::probe_manager::policy::ProbeRequestResult::Queued { job_id } = result else {
        panic!("queued job required")
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while !probe.entered() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let held_store = store.clone();
    let holder = std::thread::spawn(move || {
        let _guard =
            crate::writer_hold::WriterHold::acquire(held_store.clone(), store::Db::Library);
        locked_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    locked_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    probe.release();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = store
                .as_ref()
                .get_probe_job(&job_id)
                .unwrap()
                .unwrap()
                .status;
            if status == "succeeded" {
                break;
            }
            assert_ne!(status, "cancelled");
            assert_ne!(status, "failed");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("busy publication must be retried to completion");
    assert!(probe.entered.load(std::sync::atomic::Ordering::SeqCst) >= 2);
    manager.shutdown_workers().await.unwrap();
}

#[test]
fn configured_metadata_concurrency_runs_two_gated_probes() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    store
        .as_ref()
        .put_setting("probe.concurrency", "2")
        .unwrap();
    let first = seed_probe_candidate_in(&store);
    let second = seed_probe_candidate_in(&store);
    let probe = GatedTracksProbe::new();
    let manager = Arc::new(
        api::probe_manager::ProbeManager::new(store)
            .unwrap()
            .with_tracks_probe(probe.clone()),
    );
    runtime.block_on(async {
        assert!(manager.try_start_workers());
    });
    let observe = probe.clone();
    let queued = manager.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let measured = std::thread::spawn(move || {
        let entered = runtime.block_on(async {
            // Poll idle workers before admission: receiver ownership must not make
            // the second worker disappear while the first awaits an empty queue.
            while queued.worker_count() < 5 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            tokio::task::yield_now().await;
            queued
                .request_probe(&first, ProbeRequestOrigin::Detail)
                .unwrap();
            queued
                .request_probe(&second, ProbeRequestOrigin::Detail)
                .unwrap();
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(3) {
                if observe.entered.load(std::sync::atomic::Ordering::SeqCst) == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let entered = observe.entered.load(std::sync::atomic::Ordering::SeqCst);
            observe.release();
            entered
        });
        tx.send(entered).unwrap();
    });
    let entered = rx
        .recv_timeout(Duration::from_secs(6))
        .expect("probe concurrency watchdog");
    probe.release();
    measured.join().unwrap();
    assert_eq!(
        entered, 2,
        "both probes must enter before either gate is released"
    );
}

#[test]
fn reconfiguration_driver_leaves_timers_responsive_behind_store_contention() {
    let temp = tempfile::tempdir().unwrap();
    let (runtime, state) = boot_inside_runtime(temp.path());
    let library = state
        .store()
        .default_library(MediaKind::Movie)
        .unwrap()
        .unwrap();
    let lease = state.acquire_library_work(&library.id).unwrap();
    let accepted = runtime.block_on(async {
        api::storage_operations::begin_library_reconfiguration(
            &state,
            &library.id,
            api::storage_operations::ReconfigurationRequest {
                root_paths: None,
                display_locale: Some("ja".into()),
            },
        )
        .unwrap()
    });
    let id = accepted["operation_id"].as_str().unwrap().to_string();
    let held = state.store();
    let (locked, acquired) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = crate::writer_hold::WriterHold::acquire(held.clone(), store::Db::Library);
        locked.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(800));
    });
    acquired.recv_timeout(Duration::from_secs(2)).unwrap();
    drop(lease);
    let (tx, rx) = std::sync::mpsc::channel();
    let operation_state = state.clone();
    let operation = id.clone();
    let measured = std::thread::spawn(move || {
        let elapsed = runtime.block_on(async {
            let start = Instant::now();
            tokio::time::sleep(Duration::from_millis(10)).await;
            let elapsed = start.elapsed();
            tokio::time::timeout(Duration::from_secs(4), async {
                loop {
                    let handle = operation_state.store();
                    let operation = operation.clone();
                    let view = tokio::task::spawn_blocking(move || {
                        handle
                            .as_ref()
                            .library_operation_view(&operation)
                            .unwrap()
                            .unwrap()
                    })
                    .await
                    .unwrap();
                    if view.state == "succeeded" {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("driver must actually finish its operation");
            elapsed
        });
        tx.send(elapsed).unwrap();
    });
    let elapsed = rx
        .recv_timeout(Duration::from_secs(6))
        .expect("driver executor watchdog");
    holder.join().unwrap();
    measured.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(200),
        "driver parked timer for {elapsed:?}"
    );
    assert_eq!(
        state
            .store()
            .library_operation_view(&id)
            .unwrap()
            .unwrap()
            .state,
        "succeeded"
    );
}

#[tokio::test]
async fn shutdown_waits_for_running_probe_and_releases_capture_gate() {
    use api::fingerprint_job::CaptureGate;
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let first = seed_probe_candidate_in(&store);
    let second = seed_probe_candidate_in(&store);
    let probe = GatedTracksProbe::new();
    let manager = Arc::new(
        api::probe_manager::ProbeManager::new(store.clone())
            .unwrap()
            .with_tracks_probe(probe.clone()),
    );
    manager.try_start_workers();
    let first_job = manager
        .request_probe(&first, ProbeRequestOrigin::Detail)
        .unwrap();
    let api::probe_manager::policy::ProbeRequestResult::Queued { job_id } = first_job else {
        panic!("queued job required")
    };
    manager
        .request_probe(&second, ProbeRequestOrigin::Detail)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !probe.entered() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let gate = api::probe_manager::progress::MetadataPriorityGate::new(manager.clone());
    let waiting = tokio::spawn(async move { gate.wait_before_capture().await });
    tokio::task::yield_now().await;
    let shutdown_manager = manager.clone();
    let shutdown = tokio::spawn(async move { shutdown_manager.shutdown_workers().await });
    let second_manager = manager.clone();
    let second_shutdown = tokio::spawn(async move { second_manager.shutdown_workers().await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let waited_for_probe = !shutdown.is_finished();
    let second_waited = !second_shutdown.is_finished();
    probe.release();
    tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .expect("shutdown did not drain the active probe")
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), second_shutdown)
        .await
        .expect("concurrent shutdown did not finish")
        .unwrap()
        .unwrap();
    assert!(
        waited_for_probe,
        "shutdown must wait for the active blocking probe"
    );
    assert!(
        second_waited,
        "both shutdown callers must wait for active work"
    );
    let _replacement = api::probe_manager::ProbeManager::new(store.clone()).unwrap();
    assert_eq!(
        store
            .as_ref()
            .get_probe_job(&job_id)
            .unwrap()
            .unwrap()
            .status,
        "succeeded"
    );
    tokio::time::timeout(Duration::from_millis(200), waiting)
        .await
        .expect("cancellation must wake the capture gate")
        .unwrap();
}

#[tokio::test]
async fn shutdown_completes_when_workers_are_waiting_for_queue_items() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path()).unwrap());
    let manager = Arc::new(api::probe_manager::ProbeManager::new(store).unwrap());
    manager.try_start_workers();
    tokio::time::timeout(Duration::from_secs(2), async {
        while manager.worker_count() < 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), manager.shutdown_workers())
        .await
        .expect("idle receivers must stop promptly")
        .unwrap();
}

#[tokio::test]
async fn timed_out_shutdown_blocks_recovery_until_old_probe_exits() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let row = seed_probe_candidate_in(&store);
    let probe = GatedTracksProbe::new();
    let manager = Arc::new(
        api::probe_manager::ProbeManager::new(store.clone())
            .unwrap()
            .with_tracks_probe(probe.clone()),
    );
    assert!(manager.try_start_workers());
    let result = manager
        .request_probe(&row, ProbeRequestOrigin::Detail)
        .unwrap();
    let api::probe_manager::policy::ProbeRequestResult::Queued { job_id } = result else {
        panic!("queued job required")
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while !probe.entered() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let shutdown = tokio::time::timeout(Duration::from_secs(4), manager.shutdown_workers()).await;
    let returned_error = matches!(shutdown, Ok(Err(_)));
    let replacement_store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let replacement = Arc::new(api::probe_manager::ProbeManager::new(replacement_store).unwrap());
    let old_still_running = store.as_ref().probe_job_units(&job_id).unwrap()[0].status == "running";
    let refused_start = !replacement.try_start_workers();
    probe.release();
    assert!(
        returned_error,
        "shutdown must return an error when work cannot stop within its deadline"
    );
    assert!(
        old_still_running,
        "recovery must not reset a still executing unit"
    );
    assert!(
        refused_start,
        "a replacement manager must not overlap the old worker"
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while !replacement.try_start_workers() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("replacement did not start after the old worker exited");
    replacement.shutdown_workers().await.unwrap();
}

struct FailedTracks;
impl api::probe_manager::TracksProbe for FailedTracks {
    fn probe(
        &self,
        _: &Path,
        _: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        Err(library::LibraryError::Probe("fixture probe failure".into()))
    }
}

#[tokio::test]
async fn failed_metadata_probe_releases_capture_gate_and_finishes_job() {
    use api::fingerprint_job::CaptureGate;
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let row = seed_probe_candidate_in(&store);
    let manager = Arc::new(
        api::probe_manager::ProbeManager::new(store.clone())
            .unwrap()
            .with_tracks_probe(Arc::new(FailedTracks)),
    );
    manager.try_start_workers();
    let result = manager
        .request_probe(&row, ProbeRequestOrigin::Detail)
        .unwrap();
    let api::probe_manager::policy::ProbeRequestResult::Queued { job_id } = result else {
        panic!("queued job required")
    };
    let gate = api::probe_manager::progress::MetadataPriorityGate::new(manager.clone());
    tokio::time::timeout(Duration::from_secs(3), async {
        gate.wait_before_capture().await;
        loop {
            if store
                .as_ref()
                .get_probe_job(&job_id)
                .unwrap()
                .unwrap()
                .status
                == "failed"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("failed metadata must return its queue accounting");
    manager.shutdown_workers().await.unwrap();
}
