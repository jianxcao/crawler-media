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
