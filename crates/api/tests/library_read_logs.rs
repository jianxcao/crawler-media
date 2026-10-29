//! Production tracing must retain measurements completed after HTTP timeout.
use api::router;
use axum::http::StatusCode;
use media_server::{provider::QueryTiming, query_metrics::QueryMetrics};
use std::collections::BTreeMap;
use std::future::{Future, poll_fn};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::task::Poll;
use std::time::{Duration, Instant};
use tower::ServiceExt;
use tracing::{Event, Subscriber, field::Visit};
use tracing_subscriber::{Layer, layer::Context, prelude::*};
#[allow(dead_code)]
#[path = "library_read/support.rs"]
mod support;

type Fields = BTreeMap<String, String>;
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<Fields>>>);
struct Visitor(Fields);
impl Visit for Visitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().into(), format!("{value:?}"));
    }
}
impl<S: Subscriber> Layer<S> for Logs {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let mut visitor = Visitor(Fields::new());
        event.record(&mut visitor);
        self.0.lock().unwrap().push(visitor.0);
    }
}
fn logs() -> &'static Logs {
    static LOGS: OnceLock<Logs> = OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Logs::default();
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(logs.clone()))
            .unwrap();
        logs
    })
}
fn record(id: u64, message: &str) -> Option<Fields> {
    logs()
        .0
        .lock()
        .unwrap()
        .iter()
        .find(|fields| {
            fields.get("request_id") == Some(&id.to_string())
                && fields.get("message").is_some_and(|v| v == message)
        })
        .cloned()
}
async fn stopped(id: u64) -> Fields {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(fields) = record(id, "Library read worker actually stopped") {
                return fields;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}
fn timing() -> QueryTiming {
    let started = Instant::now();
    QueryTiming {
        started,
        deadline: started + Duration::from_millis(200),
    }
}
fn completed_duration(fields: &Fields, name: &str) -> f64 {
    fields
        .get(name)
        .unwrap_or_else(|| panic!("missing {name}: {fields:?}"))
        .strip_prefix("Some(")
        .unwrap()
        .strip_suffix(')')
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn late_sql_duration_is_pending_then_logged_after_actual_worker_exit() {
    logs();
    let fixture = support::Fixture::new(1);
    let pool = fixture.state.library_read_pool_weak().upgrade().unwrap();
    let capacity = pool.available();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    pool.set_snapshot_observer(Some(Arc::new(move || {
        entered_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
    })));
    let timing = timing();
    let metrics = QueryMetrics::new(timing, true);
    let id = metrics.snapshot().request_id;
    let request = support::request(
        &format!("/api/v1/libraries/{}/facets", fixture.library),
        timing,
        metrics,
    );
    let reader = tokio::spawn(router(fixture.state.clone()).oneshot(request));
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(2)).unwrap())
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), reader)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(pool.available(), capacity - 1, "late SQL retains its lease");
    assert_eq!(fixture.state.http_work.available_permits(), 7);
    let completion = record(id, "Library read request completed").unwrap();
    release_tx.send(()).unwrap();
    let late = stopped(id).await;
    assert_eq!(
        completion.get("sql_ms").map(String::as_str),
        Some("None"),
        "{completion:?}"
    );
    assert!(completion["pending_phases"].contains("sql"));
    assert_eq!(completion["metrics_final"], "false");
    assert!(completed_duration(&late, "sql_ms") >= 150., "{late:?}");
    assert_eq!(late["metrics_final"], "true");
    assert_eq!(late["pending_phases"], "[]");
}

#[test]
fn queued_worker_logs_actual_queue_duration_after_response_timeout() {
    logs();
    let fixture = support::Fixture::new(1);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (auth_entered_tx, auth_entered_rx) = mpsc::channel();
        let (auth_release_tx, auth_release_rx) = mpsc::channel();
        let auth_gate = tokio::task::spawn_blocking(move || {
            auth_entered_tx.send(()).unwrap();
            let _ = auth_release_rx.recv_timeout(Duration::from_secs(5));
        });
        auth_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let timing = timing();
        let metrics = QueryMetrics::new(timing, false);
        let id = metrics.snapshot().request_id;
        let mut reader = Box::pin(router(fixture.state.clone()).oneshot(support::request(
            "/Items?Limit=1",
            timing,
            metrics,
        )));
        // Queue AppReader auth behind the first gate. Keep the request unpolled
        // while auth finishes and the next blocker occupies the only thread.
        assert!(
            poll_fn(|cx| Poll::Ready(reader.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            entered_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        });
        auth_release_tx.send(()).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        auth_gate.await.unwrap();
        let response = tokio::time::timeout(Duration::from_secs(2), reader).await;
        let available = fixture.state.http_work.available_permits();
        let completion = record(id, "Library read request completed");
        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        assert_eq!(
            response.unwrap().unwrap().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            available, 7,
            "queued read retains its permit after HTTP timeout"
        );
        let completion = completion.unwrap();
        let late = stopped(id).await;
        assert_eq!(
            completion.get("queue_wait_ms").map(String::as_str),
            Some("None"),
            "{completion:?}"
        );
        assert!(completion["pending_phases"].contains("queue"));
        assert_eq!(completion["metrics_final"], "false");
        assert!(
            completed_duration(&late, "queue_wait_ms") >= 150.,
            "{late:?}"
        );
        assert_eq!(late["metrics_final"], "true");
        assert_eq!(late["pending_phases"], "[]");
        assert_eq!(fixture.state.http_work.available_permits(), 8);
    });
}

struct ReaderRelease(Arc<(Mutex<bool>, std::sync::Condvar)>);
impl Drop for ReaderRelease {
    fn drop(&mut self) {
        let (released, changed) = self.0.as_ref();
        *released.lock().unwrap() = true;
        changed.notify_all();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reader_pool_wait_deadline_is_measured_in_the_completion_log() {
    logs();
    let fixture = support::Fixture::new(1);
    let release = ReaderRelease(Arc::new((Mutex::new(false), std::sync::Condvar::new())));
    let gate = release.0.clone();
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let state = fixture
        .state
        .clone()
        .with_library_read_stage_barrier(Arc::new(move |stage| {
            if stage == "prepare" {
                entered_tx.send(()).unwrap();
                let (released, changed) = gate.as_ref();
                let guard = released.lock().unwrap();
                let (_guard, wait) = changed
                    .wait_timeout_while(guard, Duration::from_secs(5), |released| !*released)
                    .unwrap();
                assert!(!wait.timed_out(), "reader holders must be released");
            }
        }));
    let app = router(state.clone());
    let uri = format!("/api/v1/libraries/{}/facets", fixture.library);
    let mut holders = Vec::new();
    for _ in 0..8 {
        let timing = QueryTiming::start();
        holders.push(tokio::spawn(app.clone().oneshot(support::request(
            &uri,
            timing,
            QueryMetrics::new(timing, false),
        ))));
        tokio::time::timeout(Duration::from_secs(2), entered_rx.recv())
            .await
            .unwrap()
            .unwrap();
    }
    let timing = timing();
    let metrics = QueryMetrics::new(timing, false);
    let id = metrics.snapshot().request_id;
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        app.oneshot(support::request(&uri, timing, metrics)),
    )
    .await
    .unwrap()
    .unwrap();
    let completion = record(id, "Library read request completed").unwrap();
    // Releasing the other eight real readers must not start a late worker for
    // the waiter whose absolute request deadline has already elapsed.
    drop(release);
    for holder in holders {
        assert_eq!(holder.await.unwrap().unwrap().status(), StatusCode::OK);
    }
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        completed_duration(&completion, "prepare_lock_wait_ms") >= 150.,
        "{completion:?}"
    );
    assert_eq!(completion["metrics_final"], "true");
    assert_eq!(completion["pending_phases"], "[]");
    assert!(
        record(id, "Library read worker actually stopped").is_none(),
        "an expired async reader waiter never acquires a blocking worker"
    );
}
