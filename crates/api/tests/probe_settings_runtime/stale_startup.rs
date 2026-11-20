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
