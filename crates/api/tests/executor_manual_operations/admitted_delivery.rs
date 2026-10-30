use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

struct CloseAfterAdmission {
    store: OnceLock<Arc<Store>>,
    gate: OnceLock<Arc<api::write_gate::WriteGate>>,
    finish: parking_lot::Mutex<Option<std::sync::mpsc::Sender<()>>>,
    intent_seen: AtomicBool,
    added: AtomicBool,
    hold_confirmation: bool,
}
impl downloader::Downloader for CloseAfterAdmission {
    fn add(&self, torrent: &domain::Torrent) -> Result<(), downloader::DownloaderError> {
        let handle = self.store.get().unwrap();
        handle
            .writers()
            .with_writer_timeout(Duration::from_millis(100), store::Db::Subscribe, |_| Ok(()))
            .expect("add must run outside the Subscribe writer");
        let store = handle.as_ref();
        self.intent_seen.store(
            store
                .list_all_pending_routed()
                .unwrap()
                .iter()
                .any(|(_, _, pending)| {
                    pending.torrent.enclosure == torrent.enclosure && pending.submitted_at.is_none()
                }),
            Ordering::SeqCst,
        );
        let gate = self.gate.get().unwrap().clone();
        let hold_confirmation = self.hold_confirmation;
        let shared = self.store.get().unwrap().clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        *self.finish.lock() = Some(finish_tx);
        std::thread::spawn(move || {
            let _held = hold_confirmation.then(|| {
                crate::writer_hold::WriterHold::acquire(shared.clone(), store::Db::Subscribe)
            });
            let _closed = gate.close();
            ready_tx.send(()).unwrap();
            let _ = finish_rx.recv_timeout(Duration::from_secs(5));
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        self.added.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn completed_files(
        &self,
        _: &domain::Torrent,
    ) -> Result<Vec<std::path::PathBuf>, downloader::DownloaderError> {
        Ok(vec![])
    }
}

#[test]
fn admitted_add_is_tracked_and_confirmed_even_when_maintenance_starts_during_add() {
    let client = Arc::new(CloseAfterAdmission {
        store: OnceLock::new(),
        gate: OnceLock::new(),
        finish: Default::default(),
        intent_seen: AtomicBool::new(false),
        added: AtomicBool::new(false),
        hold_confirmation: false,
    });
    let f = Fixture::build(Some(client.clone()));
    client.store.set(f.state.store()).ok().unwrap();
    client.gate.set(f.state.write_gate()).ok().unwrap();
    let response = runtime()
        .block_on(f.app.clone().oneshot(f.request(0)))
        .unwrap();
    let status = response.status();
    let body = runtime().block_on(common::json_body(response));
    client.finish.lock().take().unwrap().send(()).unwrap();
    assert!(client.added.load(Ordering::SeqCst));
    assert_eq!(
        status,
        StatusCode::OK,
        "accepted add must finish tracking: {body}"
    );
    assert!(
        client.intent_seen.load(Ordering::SeqCst),
        "persist routed intent before outbound add"
    );
    let pending = f
        .state
        .store()
        .load_pending(f.subscribe.parse().unwrap())
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].1.submitted_at.is_some());
}

#[test]
fn confirmation_busy_retains_the_routed_unconfirmed_intent_after_successful_add() {
    let client = Arc::new(CloseAfterAdmission {
        store: OnceLock::new(),
        gate: OnceLock::new(),
        finish: Default::default(),
        intent_seen: AtomicBool::new(false),
        added: AtomicBool::new(false),
        hold_confirmation: true,
    });
    let f = Fixture::build(Some(client.clone()));
    client.store.set(f.state.store()).ok().unwrap();
    client.gate.set(f.state.write_gate()).ok().unwrap();
    let response = runtime()
        .block_on(f.app.clone().oneshot(f.request(0)))
        .unwrap();
    client.finish.lock().take().unwrap().send(()).unwrap();
    let status = response.status();
    let retry_after = response.headers().get("retry-after").cloned();
    let body = runtime().block_on(common::json_body(response));
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "runtime.busy");
    assert_eq!(retry_after.unwrap(), "1");
    assert!(client.added.load(Ordering::SeqCst));
    assert!(client.intent_seen.load(Ordering::SeqCst));
    let handle = f.state.store();
    let store = handle.as_ref();
    let pending = store.load_pending(f.subscribe.parse().unwrap()).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(
        pending[0].1.submitted_at.is_none(),
        "busy confirmation keeps the recoverable intent"
    );
    assert_eq!(
        pending[0].1.torrent.enclosure,
        "https://pt.example/download.php?id=1&passkey=abc"
    );
}

struct PausedAdd {
    entered: tokio::sync::Notify,
    release: parking_lot::Mutex<std::sync::mpsc::Receiver<()>>,
}
impl downloader::Downloader for PausedAdd {
    fn add(&self, _: &domain::Torrent) -> Result<(), downloader::DownloaderError> {
        self.entered.notify_one();
        self.release
            .lock()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        Ok(())
    }
    fn completed_files(
        &self,
        _: &domain::Torrent,
    ) -> Result<Vec<std::path::PathBuf>, downloader::DownloaderError> {
        Ok(vec![])
    }
}

#[test]
fn cancelled_admit_keeps_worker_capacity_until_remote_result_is_confirmed() {
    let (release, receive) = std::sync::mpsc::channel();
    let client = Arc::new(PausedAdd {
        entered: tokio::sync::Notify::new(),
        release: parking_lot::Mutex::new(receive),
    });
    let mut f = Fixture::build(Some(client.clone()));
    let budget = Arc::new(api::blocking_budget::BlockingBudget::new(1));
    f.state = f.state.with_http_work(budget.clone());
    f.app = router(f.state.clone());
    runtime().block_on(async {
        let request = f.request(0);
        let app = f.app.clone();
        let task = tokio::spawn(async move { app.oneshot(request).await });
        tokio::time::timeout(Duration::from_secs(3), client.entered.notified())
            .await
            .unwrap();
        let pending = f
            .state
            .store()
            .load_pending(f.subscribe.parse().unwrap())
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].1.submitted_at.is_none());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(budget.available_permits(), 0);
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while budget.available_permits() == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    });
    let pending = f
        .state
        .store()
        .load_pending(f.subscribe.parse().unwrap())
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].1.submitted_at.is_some());
}
