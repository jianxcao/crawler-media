use super::*;
use axum::body::Body;
use axum::http::Request;
use media_server::provider::QueryTiming;
use std::time::Instant;
use tower::ServiceExt;

async fn wait_for_recorded_probe(fixture: &Fixture, expected: bool) {
    let ledger = fixture.files[0].file_id.to_string();
    let started = Instant::now();
    loop {
        let recorded = fixture
            .state
            .store()
            .lock()
            .probe_unit_recorded_for_ledger(&ledger)
            .unwrap();
        if recorded == expected && (expected || started.elapsed() >= Duration::from_millis(150)) {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_millis(700),
            "expected durable probe presence={expected}, got {recorded}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_page_transfers_probe_admission_until_writer_is_released() {
    let fixture = Fixture::new(1);
    let uri = fixture.items_uri();
    let writer = HeldWriter::acquire(&fixture.state);
    let (status, body) = tokio::time::timeout(
        Duration::from_millis(500),
        get_json(api::router(fixture.state.clone()), &uri),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["Items"].as_array().unwrap().len(), 1);
    drop(writer);
    wait_for_recorded_probe(&fixture, true).await;
}

struct ValidationPause {
    release: Arc<(Mutex<bool>, std::sync::Condvar)>,
}
impl Drop for ValidationPause {
    fn drop(&mut self) {
        let (released, changed) = self.release.as_ref();
        *released.lock().unwrap() = true;
        changed.notify_all();
    }
}

fn pause_after_probe_assessment(
    fixture: &mut Fixture,
) -> (ValidationPause, tokio::sync::oneshot::Receiver<()>) {
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let gate = release.clone();
    let probe = fixture.state.probe.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = Mutex::new(Some(tx));
    fixture.state = fixture
        .state
        .clone()
        .with_library_read_stage_barrier(Arc::new(move |stage| {
            if stage != "validate" || probe.assessments.load(Ordering::SeqCst) == 0 {
                return;
            }
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            let (released, changed) = gate.as_ref();
            let guard = released.lock().unwrap();
            let (_guard, wait) = changed
                .wait_timeout_while(guard, Duration::from_secs(3), |flag| !*flag)
                .unwrap();
            assert!(!wait.timed_out(), "validation pause was not released");
        }));
    (ValidationPause { release }, rx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_page_does_not_persist_a_late_probe_after_writer_release() {
    let mut fixture = Fixture::new(1);
    let uri = fixture.items_uri();
    let (validation, entered) = pause_after_probe_assessment(&mut fixture);
    let writer = HeldWriter::acquire(&fixture.state);
    let app = api::router(fixture.state.clone());
    let request = Request::get(uri)
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let task = tokio::spawn(app.oneshot(request));
    tokio::time::timeout(Duration::from_millis(500), entered)
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(validation);
    drop(writer);
    wait_for_recorded_probe(&fixture, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_page_does_not_persist_a_late_probe_after_writer_release() {
    let mut fixture = Fixture::new(1);
    let uri = fixture.items_uri();
    let (validation, entered) = pause_after_probe_assessment(&mut fixture);
    let writer = HeldWriter::acquire(&fixture.state);
    let started = Instant::now();
    let mut request = Request::get(uri)
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(QueryTiming {
        started,
        deadline: started + Duration::from_millis(150),
    });
    let task = tokio::spawn(api::router(fixture.state.clone()).oneshot(request));
    tokio::time::timeout(Duration::from_millis(500), entered)
        .await
        .unwrap()
        .unwrap();
    let response = task.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(validation);
    drop(writer);
    wait_for_recorded_probe(&fixture, false).await;
}
