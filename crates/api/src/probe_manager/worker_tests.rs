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
    assert!(store.start_probe_unit(job, file).unwrap());
}

#[tokio::test]
async fn driver_step_waits_for_blocking_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(parking_lot::Mutex::new(
        store::Store::open(directory.path()).unwrap(),
    ));
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
    let store = Arc::new(parking_lot::Mutex::new(
        store::Store::open(directory.path()).unwrap(),
    ));
    let manager = Arc::new(ProbeManager::new(store.clone()).unwrap());
    add_running_job(&store.lock(), "first", "file-1");
    add_running_job(&store.lock(), "second", "file-2");
    manager.seen.lock().insert("file-1".into());
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let held_store = store.clone();
    let holder = std::thread::spawn(move || {
        let _guard = held_store.lock();
        held_tx.send(()).unwrap();
        release_rx.recv().unwrap();
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
    let store = store.lock();
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
    let store = Arc::new(parking_lot::Mutex::new(
        store::Store::open(directory.path()).unwrap(),
    ));
    let manager = Arc::new(ProbeManager::new(store.clone()).unwrap());
    add_running_job(&store.lock(), "old", "file");
    store.lock().cancel_probe_job("old", "cancelled").unwrap();
    add_running_job(&store.lock(), "new", "file");
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
        store.lock().probe_job_units("new").unwrap()[0].status,
        "running"
    );
}
