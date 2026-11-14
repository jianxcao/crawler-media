use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

struct Invalidate {
    s: Arc<store::Store>,
    target: domain::FileCoverage,
    reference: domain::FileCoverage,
    cancel: bool,
    refresh: bool,
    changed: AtomicBool,
    released: parking_lot::Mutex<bool>,
    resume: parking_lot::Condvar,
}
impl FingerprintEngine for Invalidate {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(
        &self,
        a: &[u32],
        b: &[u32],
        min: f32,
        max: f32,
    ) -> Option<CommonSegment> {
        if !self.changed.swap(true, Ordering::SeqCst) {
            if self.cancel {
                let job = if self.refresh {
                    self.s
                        .latest_marker_refresh_for_season(&self.target.file.media_id.to_string(), 1)
                        .unwrap()
                } else {
                    self.s
                        .latest_probe_job_for_scope(&format!("ledger:{}", self.target.file.file_id))
                        .unwrap()
                }
                .unwrap();
                self.s
                    .cancel_probe_job(&job.id, "cancel during kind analysis")
                    .unwrap();
            } else {
                std::fs::write(
                    &self.reference.file.path,
                    "http://127.0.0.1:8080/new-reference.mkv\n",
                )
                .unwrap();
            }
            let mut released = self.released.lock();
            while !*released {
                self.resume.wait(&mut released);
            }
        }
        KindEngine::default().find_common_segment(a, b, min, max)
    }
}
async fn rejected(cancel: bool, refresh: bool) {
    let mut s = scenario("full_window").await;
    s.manager.shutdown_workers().await.unwrap();
    let engine = Arc::new(Invalidate {
        s: s.store.clone(),
        target: s.ledgers[0].clone(),
        reference: s.ledgers[1].clone(),
        cancel,
        refresh,
        changed: AtomicBool::new(false),
        released: parking_lot::Mutex::new(false),
        resume: parking_lot::Condvar::new(),
    });
    s.manager = Arc::new(
        ProbeManager::with_engines(
            s.store.clone(),
            engine.clone(),
            Arc::new(KindEngine::default()),
        )
        .unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
    if refresh {
        assert_eq!(
            s.manager
                .enqueue_marker_refresh(s.ledgers.iter().map(unit).collect()),
            8
        );
    } else {
        assert!(s.manager.enqueue(unit(&s.ledgers[0])));
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !engine.changed.load(Ordering::SeqCst) {
            s.clock.set(s.clock.now_ms() + 2_000);
            s.manager.wake_dispatch();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // Poll shutdown before releasing comparison, so a later valid generation
    // cannot race this assertion about the single stale prepared transaction.
    let shutdown = s.manager.shutdown_workers();
    tokio::pin!(shutdown);
    std::future::poll_fn(|cx| {
        use std::future::Future;
        assert!(shutdown.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    *engine.released.lock() = true;
    engine.resume.notify_all();
    shutdown.await.unwrap();
    for row in &s.ledgers {
        assert!(
            s.store
                .get_file_marker(row.file.file_id, row.units[0])
                .unwrap()
                .is_none(),
            "rejected prepared partial must not publish any target"
        );
        assert!(
            s.store
                .current_chapter_frame_task(row.file.file_id)
                .unwrap()
                .is_none()
        );
    }
    let job = if refresh {
        s.store
            .latest_marker_refresh_for_season(&s.media_id.to_string(), 1)
            .unwrap()
    } else {
        s.store
            .latest_probe_job_for_scope(&format!("ledger:{}", s.ledgers[0].file.file_id))
            .unwrap()
    }
    .unwrap();
    assert_ne!(job.status, "succeeded");
    assert!(
        s.store
            .probe_job_units(&job.id)
            .unwrap()
            .iter()
            .all(|unit| unit.detection_outcome_json.is_none())
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_ordinary_partial_cannot_publish() {
    rejected(true, false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_refresh_partial_cannot_publish() {
    rejected(true, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_reference_rejects_ordinary_partial() {
    rejected(false, false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_reference_rejects_atomic_refresh_partial() {
    rejected(false, true).await;
}
