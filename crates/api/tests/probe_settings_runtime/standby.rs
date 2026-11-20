use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn promoted_standby_uses_recent_remote_pause() {
    let _serial = PROCESS_GATE.lock().await;
    let root = tempfile::tempdir().unwrap();
    let primary = state(root.path());
    let standby = api::ApiState::new_arc_with_admin_bootstrap_writable(
        primary.store(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(root.path().join("dl"))),
        api::bootstrap_credentials::AdminBootstrapAction::Keep {
            cli_token: "management-secret".into(),
        },
        false,
    )
    .unwrap();
    remote_reads_request(
        &api::router(primary.clone()),
        "PUT",
        json!({"paused": true}),
    )
    .await;
    assert_eq!(
        remote_reads_request(&api::router(primary.clone()), "GET", Value::Null).await,
        json!({"paused": true})
    );
    primary.probe.shutdown_workers().await.unwrap();
    assert_eq!(
        remote_reads_request(&api::router(standby.clone()), "GET", Value::Null).await,
        json!({"paused": true})
    );
    standby.ensure_writable_runtime_started().unwrap();

    let path = root.path().join("movie.strm");
    let stop = marker::WorkControl::default();
    let waiter = tokio::task::spawn_blocking({
        let stop = stop.clone();
        move || marker::media_read::acquire_media_read(&path, &stop).is_ok()
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    stop.cancel();
    let admitted = waiter.await.unwrap();
    standby.probe.shutdown_workers().await.unwrap();
    assert!(!admitted, "takeover ignored the saved pause");
}
