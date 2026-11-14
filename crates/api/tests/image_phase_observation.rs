//! Public image delivery emits correlated, bounded diagnostics on timeout.
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use api::{ApiState, Store};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use tower::ServiceExt;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::{Layer, layer::Context, prelude::*};

const IMAGE: &[u8] = b"\x89PNG\r\n\x1a\nfinite image bytes";
const SECRET: &str = "image-phase-secret-739251";

struct Offline;
impl Fetcher for Offline {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<BTreeMap<String, String>>>>);
#[derive(Default)]
struct Fields(BTreeMap<String, String>);
impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().into(), format!("{value:?}"));
    }
}
impl<S: Subscriber> Layer<S> for Events {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        if event.metadata().target() == "api::image_delivery" {
            let mut fields = Fields::default();
            event.record(&mut fields);
            self.0.lock().unwrap().push(fields.0);
        }
    }
}

#[derive(Default)]
struct Gate {
    state: Mutex<(usize, bool)>,
    changed: Condvar,
}
impl Gate {
    fn fetch(&self) -> Result<(String, Vec<u8>), String> {
        let mut state = self.state.lock().unwrap();
        state.0 += 1;
        self.changed.notify_all();
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(6), |state| !state.1)
            .unwrap();
        if timeout.timed_out() && !state.1 {
            return Err("finite fetch deadline".into());
        }
        Ok(("image/png".into(), IMAGE.to_vec()))
    }
    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
}
struct Release(Arc<Gate>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn request(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn timeout_and_late_fill_completion_have_correlated_redacted_phases() {
    // This integration binary has one test, so a global subscriber also sees
    // detached Tokio and blocking workers without relying on thread-local state.
    let events = Events::default();
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(events.clone()))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let gate = Arc::new(Gate::default());
    let _release = Release(gate.clone());
    let origin = gate.clone();
    let state = ApiState::new(
        store,
        "phase-test".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(Offline),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
    )
    .unwrap()
    .with_image_fetch(Arc::new(move |_| origin.fetch()));
    let app = api::router(state);
    let uri = format!(
        "/api/v1/images/proxy?url=https%3A%2F%2Fimage.tmdb.org%2Ft%2Fp%2Fw185%2F{SECRET}.png%3Ftoken%3D{SECRET}"
    );
    let pending = tokio::spawn({
        let app = app.clone();
        let uri = uri.clone();
        async move { app.oneshot(request(&uri)).await.unwrap() }
    });
    let entered = tokio::time::timeout(Duration::from_secs(1), async {
        while gate.state.lock().unwrap().0 == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let first = tokio::time::timeout(Duration::from_secs(3), pending).await;
    gate.release();
    entered.expect("origin was not entered");
    let first = first.expect("observer did not finish").unwrap();
    assert_eq!(first.status(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(first.headers()["cache-control"], "no-store");
    let body = to_bytes(first.into_body(), 1024).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], "image.timeout");

    let complete = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = events.0.lock().unwrap().clone();
            if snapshot.iter().any(|event| {
                event
                    .get("event")
                    .is_some_and(|v| v == "image_fill_complete")
            }) {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("timed-out fill did not log completion");
    let timeouts: Vec<_> = complete
        .iter()
        .filter(|e| {
            e.get("event")
                .is_some_and(|v| v == "image_delivery_timeout")
        })
        .collect();
    assert_eq!(timeouts.len(), 1);
    let timeout = timeouts[0];
    assert_eq!(timeout.get("role").map(String::as_str), Some("leader"));
    assert_eq!(
        timeout.get("phase").map(String::as_str),
        Some("upstream_request")
    );
    for field in ["observer_elapsed_ms", "phase_elapsed_ms"] {
        timeout[field].parse::<u64>().unwrap();
    }
    let fill_id = &timeout["fill_id"];
    let observer_id = &timeout["observer_id"];
    assert!(!fill_id.is_empty() && !observer_id.is_empty());
    let completions: Vec<_> = complete
        .iter()
        .filter(|e| {
            e.get("event").is_some_and(|v| v == "image_fill_complete")
                && e.get("fill_id") == Some(fill_id)
        })
        .collect();
    assert_eq!(completions.len(), 1);
    assert_eq!(
        completions[0].get("outcome").map(String::as_str),
        Some("success")
    );
    completions[0]["fill_elapsed_ms"].parse::<u64>().unwrap();
    let serialized = format!("{complete:?}");
    assert!(!serialized.contains(SECRET));
    assert!(!serialized.contains("image.tmdb.org"));

    let cached = app.oneshot(request(&uri)).await.unwrap();
    assert_eq!(cached.status(), StatusCode::OK);
    assert_eq!(cached.headers()["cache-control"], "public, max-age=86400");
    assert_eq!(cached.headers()["content-type"], "image/png");
    assert_eq!(
        &to_bytes(cached.into_body(), 1024).await.unwrap()[..],
        IMAGE
    );
    shared_gate_timeout(&events).await;
}

async fn shared_gate_timeout(events: &Events) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let gate = Arc::new(Gate::default());
    let _release = Release(gate.clone());
    let origin = gate.clone();
    let state = ApiState::new(
        store,
        "shared-phase-test".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(Offline),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
    )
    .unwrap()
    .with_image_fetch(Arc::new(move |_| origin.fetch()));
    let app = api::router(state.clone());
    let baseline = events.0.lock().unwrap().len();
    let mut occupied = Vec::new();
    for index in 0..8 {
        let app = app.clone();
        occupied.push(tokio::spawn(async move {
            let uri = format!(
                "/api/v1/images/proxy?url=https%3A%2F%2Fimage.tmdb.org%2Foccupied-{index}.png"
            );
            app.oneshot(request(&uri)).await.unwrap()
        }));
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while gate.state.lock().unwrap().0 < 8 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("origins did not occupy all image slots");
    let uri = "/api/v1/images/proxy?url=https%3A%2F%2Fimage.tmdb.org%2Fqueued-shared.png";
    let leader = tokio::spawn({
        let app = app.clone();
        async move { app.oneshot(request(uri)).await.unwrap() }
    });
    tokio::time::sleep(Duration::from_millis(250)).await;
    let joined = tokio::spawn({
        let app = app.clone();
        async move { app.oneshot(request(uri)).await.unwrap() }
    });
    let leader = tokio::time::timeout(Duration::from_secs(3), leader)
        .await
        .expect("queued leader stalled")
        .unwrap();
    let joined = tokio::time::timeout(Duration::from_secs(1), joined)
        .await
        .expect("joined observer stalled")
        .unwrap();
    gate.release();
    for task in occupied {
        task.await.unwrap();
    }
    assert_occupied_recovery(events, baseline, state).await;
    for response in [leader, joined] {
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let snapshot = events.0.lock().unwrap()[baseline..].to_vec();
    let joined: Vec<_> = snapshot
        .iter()
        .filter(|event| {
            event
                .get("event")
                .is_some_and(|v| v == "image_delivery_timeout")
                && event.get("role").is_some_and(|v| v == "joined")
        })
        .collect();
    assert_eq!(joined.len(), 1, "events: {snapshot:#?}");
    assert_eq!(
        joined[0].get("timeout_source").map(String::as_str),
        Some("shared_fill_result"),
        "events: {snapshot:#?}"
    );
    assert_eq!(
        joined[0]
            .get("observer_deadline_expired")
            .map(String::as_str),
        Some("false"),
        "events: {snapshot:#?}"
    );
    let fill_id = &joined[0]["fill_id"];
    let observer_id = &joined[0]["observer_id"];
    assert_ne!(fill_id, "0", "events: {snapshot:#?}");
    assert!(!observer_id.is_empty(), "events: {snapshot:#?}");
    assert_eq!(
        snapshot
            .iter()
            .filter(|event| event.get("observer_id") == Some(observer_id))
            .count(),
        1,
        "events: {snapshot:#?}"
    );
    let completions: Vec<_> = snapshot
        .iter()
        .filter(|event| {
            event
                .get("event")
                .is_some_and(|v| v == "image_fill_complete")
                && event.get("fill_id") == Some(fill_id)
        })
        .collect();
    assert_eq!(completions.len(), 1, "events: {snapshot:#?}");
    assert_eq!(
        completions[0].get("outcome").map(String::as_str),
        Some("timeout")
    );
}

async fn assert_occupied_recovery(events: &Events, baseline: usize, state: ApiState) {
    let snapshot = events.0.lock().unwrap()[baseline..].to_vec();
    let ids: std::collections::BTreeSet<_> = snapshot
        .iter()
        .filter(|event| {
            event
                .get("event")
                .is_some_and(|v| v == "image_delivery_timeout")
                && event.get("role").is_some_and(|v| v == "leader")
                && event.get("phase").is_some_and(|v| v == "upstream_request")
        })
        .map(|event| event["fill_id"].clone())
        .collect();
    assert_eq!(ids.len(), 8, "occupied timeout events: {snapshot:#?}");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let all_complete = {
                let snapshot = events.0.lock().unwrap();
                let completed: std::collections::BTreeSet<_> = snapshot[baseline..]
                    .iter()
                    .filter(|event| {
                        event
                            .get("event")
                            .is_some_and(|v| v == "image_fill_complete")
                            && event.get("outcome").is_some_and(|v| v == "success")
                    })
                    .filter_map(|event| event.get("fill_id"))
                    .filter(|id| ids.contains(*id))
                    .cloned()
                    .collect();
                completed == ids
            };
            if all_complete {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("occupied image fills did not finish publication");
    let offline = state.with_image_fetch(Arc::new(|_| Err("offline origin".into())));
    let app = api::router(offline);
    for index in 0..8 {
        let uri =
            format!("/api/v1/images/proxy?url=https%3A%2F%2Fimage.tmdb.org%2Foccupied-{index}.png");
        let response = app.clone().oneshot(request(&uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "public, max-age=86400");
        assert_eq!(
            &to_bytes(response.into_body(), 1024).await.unwrap()[..],
            IMAGE
        );
    }
}
