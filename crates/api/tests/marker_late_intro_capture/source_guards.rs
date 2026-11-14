use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_source_or_cancellation_during_late_capture_cannot_publish() {
    for cancel in [false, true] {
        let (s, engine) = scenario(3, 330_000, 410_000, None).await;
        let store = s.store.clone();
        let rows = s.ledgers.clone();
        let changed = Arc::new(AtomicBool::new(false));
        let observed = changed.clone();
        *engine.on_capture.lock().unwrap() = Some(Arc::new(move |request| {
            if request.window.start_ms != 120_000 || observed.swap(true, Ordering::SeqCst) {
                return;
            }
            let row = rows.iter().find(|r| r.file.path == request.path).unwrap();
            let active = store
                .active_probe_unit_for_ledger(&row.file.file_id.to_string())
                .unwrap()
                .unwrap();
            if cancel {
                store
                    .cancel_probe_job(&active.job_id, "cancel expanded capture")
                    .unwrap();
            } else {
                std::fs::write(
                    &row.file.path,
                    "http://127.0.0.1:8080/replaced-source.mkv\n",
                )
                .unwrap();
            }
        }));
        let job = run(&s).await;
        assert!(changed.load(Ordering::SeqCst));
        if cancel {
            assert_eq!(job.status, "cancelled");
        }
        for row in &s.ledgers {
            assert!(
                s.store
                    .get_file_marker(row.file.file_id, row.units[0])
                    .unwrap()
                    .is_none()
            );
            assert!(
                s.store
                    .current_chapter_frame_task(row.file.file_id)
                    .unwrap()
                    .is_none()
            );
        }
        let attempts = s.store.list_fingerprint_attempts_for_job(&job.id).unwrap();
        assert!(attempts.iter().all(|a| a.window_start_ms != 300_000));
        assert!(s.store.recover_probe_jobs().unwrap().is_empty());
    }
}
