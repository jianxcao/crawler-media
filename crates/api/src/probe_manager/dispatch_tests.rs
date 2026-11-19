use super::*;
use crate::fingerprint_job::CaptureGate;
use std::sync::{Arc, atomic::Ordering};

#[test]
fn metadata_dispatch_handoff_finishes_before_sender_returns() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(store::Store::open(tmp.path()).unwrap());
    let manager = Arc::new(ProbeManager::new(store).unwrap());
    let (tx, mut rx) = mpsc::channel(1);
    let (published, receive) = std::sync::mpsc::channel();
    let (finished, completion) = std::sync::mpsc::channel();
    let consumer = manager.clone();
    let worker = std::thread::spawn(move || {
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        rx.try_recv().unwrap();
        consumer.metadata_stage_finished();
        finished.send(()).unwrap();
    });
    let id = ProbeDispatchId {
        job_id: "job".into(),
        ledger_id: "file".into(),
        stage: ProbeDispatchStage::Metadata,
    };
    manager
        .publish_metadata_observed(&tx, id, || {
            published.send(()).unwrap();
            completion
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
        })
        .unwrap();
    worker.join().unwrap();
    let gate = super::super::progress::MetadataPriorityGate::new(manager.clone());
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                gate.wait_before_capture(),
            )
            .await
            .expect("capture gate must observe the completed handoff");
        });
    assert_eq!(manager.metadata_pending.load(Ordering::Acquire), 0);
}

#[test]
fn metadata_dispatch_full_and_closed_keep_accounting_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = ProbeManager::new(Arc::new(store::Store::open(tmp.path()).unwrap())).unwrap();
    let (tx, rx) = mpsc::channel(1);
    let id = || ProbeDispatchId {
        job_id: "job".into(),
        ledger_id: "file".into(),
        stage: ProbeDispatchStage::Metadata,
    };
    manager.publish_metadata_observed(&tx, id(), || {}).unwrap();
    assert!(manager.publish_metadata_observed(&tx, id(), || {}).is_err());
    assert_eq!(manager.metadata_pending.load(Ordering::Acquire), 1);
    drop(rx);
    assert!(manager.publish_metadata_observed(&tx, id(), || {}).is_err());
    assert_eq!(manager.metadata_pending.load(Ordering::Acquire), 1);
    manager.metadata_stage_finished();
    assert_eq!(manager.metadata_pending.load(Ordering::Acquire), 0);
}

fn fill_metadata_window(store: &store::Store, manager: &ProbeManager) {
    for i in 0..METADATA_WINDOW {
        let ledger_id = format!("fill-{i:03}");
        let job_id = format!("job-fill-{i:03}");
        store
            .create_probe_job(
                &job_id,
                "media_probe",
                "media",
                None,
                &ledger_id,
                &[store::ProbeJobUnitSpec {
                    ledger_id: &ledger_id,
                    kind: "movie",
                    force_fingerprint: false,
                    reuse_fingerprint_cache: false,
                    overwrite_markers: false,
                    reuse_media_info_cache: false,
                }],
            )
            .unwrap();
        let id = ProbeDispatchId {
            job_id,
            ledger_id,
            stage: ProbeDispatchStage::Metadata,
        };
        manager
            .publish_metadata_observed(&manager.metadata_tx, id, || {})
            .unwrap();
    }
}

fn enqueue_backlog_unit(
    store: &store::Store,
    manager: &ProbeManager,
    job_id: &str,
    scope: &str,
    path: &str,
) -> Result<(), ProbeUnit> {
    let ledger_id = domain::LedgerId::new().to_string();
    store
        .create_probe_job(
            job_id,
            "media_probe",
            "media",
            None,
            scope,
            &[store::ProbeJobUnitSpec {
                ledger_id: &ledger_id,
                kind: "movie",
                force_fingerprint: false,
                reuse_fingerprint_cache: false,
                overwrite_markers: false,
                reuse_media_info_cache: false,
            }],
        )
        .unwrap();
    let mut cov = super::super::test_support::coverage(domain::MediaId::new(), path, None, None);
    cov.file.file_id = ledger_id.parse().unwrap();
    manager.queue_metadata(ProbeUnit {
        row: cov,
        kind: domain::MediaKind::Movie,
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: false,
        marker_refresh_id: None,
        job_id: Some(job_id.into()),
    })
}

#[test]
fn overflowed_metadata_window_preserves_fifo_order_and_blocks_live_line_jumping() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(store::Store::open(tmp.path()).unwrap());
    let manager = Arc::new(ProbeManager::new(store.clone()).unwrap());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // Fill the entire 64-slot metadata channel first.
    fill_metadata_window(store.as_ref(), &manager);

    // Older SQLite backlog item arrives first while full -> stays in SQLite, sets overflow flag.
    assert!(
        enqueue_backlog_unit(
            store.as_ref(),
            &manager,
            "job-1-older",
            "older",
            "/tmp/older.mkv"
        )
        .is_err()
    );

    // Drain only 1 slot from the channel while SQLite backlog (`job-1-older`) is STILL present.
    {
        let mut rx = manager.metadata_rx.lock().take().unwrap();
        let first = rt.block_on(rx.recv()).unwrap();
        assert!(
            store
                .as_ref()
                .start_probe_unit(
                    &first.job_id,
                    &first.ledger_id,
                    store::ProbeDispatchStage::Metadata
                )
                .unwrap()
        );
        *manager.metadata_rx.lock() = Some(rx);
    }

    // A brand-new live arrival must NOT jump into that freed channel slot ahead of `job-1-older`!
    assert!(
        enqueue_backlog_unit(
            store.as_ref(),
            &manager,
            "job-2-newer",
            "newer",
            "/tmp/newer.mkv"
        )
        .is_err(),
        "live arrival must stay in SQLite backlog instead of jumping ahead of overflowed FIFO queue"
    );

    // Drain remaining 63 channel items, then refill and verify `job-1-older` precedes `job-2-newer`.
    {
        let mut rx = manager.metadata_rx.lock().take().unwrap();
        for _ in 0..(METADATA_WINDOW - 1) {
            let item = rt.block_on(rx.recv()).unwrap();
            assert!(
                store
                    .as_ref()
                    .start_probe_unit(
                        &item.job_id,
                        &item.ledger_id,
                        store::ProbeDispatchStage::Metadata
                    )
                    .unwrap()
            );
            manager.metadata_stage_finished();
        }
        *manager.metadata_rx.lock() = Some(rx);
    }
    manager.fill_dispatch_windows();
    let mut rx = manager.metadata_rx.lock().take().unwrap();
    let mut drained = Vec::new();
    while let Ok(item) = rx.try_recv() {
        drained.push(item.job_id);
        manager.metadata_stage_finished();
    }
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0], "job-1-older");
    assert_eq!(drained[1], "job-2-newer");
}
