//! Requests admitted before a mode change must recheck admission under the writer lock.
//!
//! Fixture construction and writer holding run outside the executor. The request
//! future is polled until it has dispatched its write, so no sleep guesses when
//! ingress admission happened.

#[path = "management/common.rs"]
mod common;

use std::collections::HashMap;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use api::{ApiState, Store, router};
use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind, UserId};
use downloader::MemoryDownloader;
use futures_core::Stream;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "management-secret";
const DEVICE: &str = "write-admission-device";
const HISTORY_ID: &str = "write-admission-history";

struct AdvancingClock {
    now: AtomicI64,
    calls: AtomicUsize,
}

impl api::playback_events::PlaybackClock for AdvancingClock {
    fn now_ms(&self) -> i64 {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.now.fetch_add(1_000, Ordering::SeqCst)
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: ApiState,
    media: MediaId,
    user: UserId,
    clock: Arc<AdvancingClock>,
    history_key: String,
    history_value: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let clock = Arc::new(AdvancingClock {
            now: AtomicI64::new(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as i64,
            ),
            calls: AtomicUsize::new(0),
        });
        let state = common::state(
            dir.path(),
            Arc::new(common::Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        )
        .with_catalog(Arc::new(api::catalog::EmptyCatalog))
        .with_playback_clock(clock.clone());
        let media = MediaId::new();
        let (root, user) = {
            let handle = state.store();
            let store = handle.lock();
            let library = store.default_library(MediaKind::Movie).unwrap().unwrap();
            (
                library.root_paths[0].clone(),
                store.list_users().unwrap()[0].id,
            )
        };
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("write-admission.mkv");
        std::fs::write(&path, b"published fixture").unwrap();
        let history_key = format!("user.{user}.search.history");
        let history_value = json!([{"id": HISTORY_ID, "query": "original"}]).to_string();
        {
            let handle = state.store();
            let store = handle.lock();
            store
                .insert_media(&Media {
                    id: media,
                    kind: MediaKind::Movie,
                    title: "Write admission fixture".into(),
                    year: None,
                    original_title: None,
                    aliases: Vec::new(),
                    tmdb_id: None,
                    douban_id: None,
                    tvdb_id: None,
                    bangumi_id: None,
                    anilist_id: None,
                })
                .unwrap();
            store::test_support::publish_fixture_file(
                &store,
                &store::test_support::FixtureFile {
                    media_id: media,
                    path: path.display().to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
            store.put_setting(&history_key, &history_value).unwrap();
            store
                .insert_search_snapshot(HISTORY_ID, "torrents", "original", "{}", 1)
                .unwrap();
        }
        Self {
            _dir: dir,
            state,
            media,
            user,
            clock,
            history_key,
            history_value,
        }
    }

    fn session(&self) -> store::SessionRow {
        self.state
            .store()
            .lock()
            .get_session(self.user, DEVICE)
            .unwrap()
            .unwrap()
    }

    fn heartbeat(&self, session: &str, event: &str, sequence: i64, position: i64) -> Value {
        json!({
            "media_item_id": self.media.to_string(), "device_id": DEVICE,
            "play_session_id": session, "event": event, "event_sequence": sequence,
            "position_ms": position, "duration_ms": 7_200_000,
        })
    }
}

/// The guard lives on a dedicated thread; async code holds only its release handle.
struct HeldWriter {
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HeldWriter {
    fn new(store: Arc<Mutex<Store>>) -> Self {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _guard = store.lock();
            entered_tx.send(()).unwrap();
            // Disconnect also releases the writer during assertion unwinding.
            let _ = release_rx.recv();
        });
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        Self {
            release: Some(release_tx),
            thread: Some(thread),
        }
    }
}

impl Drop for HeldWriter {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

async fn poll_until_write_dispatched<F: Future>(
    mut request: Pin<&mut F>,
    after_ingress: impl Fn() -> bool,
) {
    tokio::time::timeout(
        Duration::from_secs(3),
        poll_fn(|cx| match request.as_mut().poll(cx) {
            Poll::Ready(_) => panic!("request completed before its held writer was released"),
            Poll::Pending if after_ingress() => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }),
    )
    .await
    .expect("request must pass ingress and dispatch its write while writer is held");
}

async fn start_playback(fixture: &Fixture, app: &axum::Router) -> String {
    let response = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/playback/sessions",
            Some(TOKEN),
            json!({"media_item_id": fixture.media.to_string(), "device_id": DEVICE}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let session = common::json_data(response).await["play_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/playback/progress",
            Some(TOKEN),
            fixture.heartbeat(&session, "start", 0, 1_000),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.session().last_event_sequence, Some(0));
    session
}

async fn assert_refusal(response: axum::response::Response, maintenance: bool) {
    assert_eq!(
        response.status(),
        if maintenance {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::CONFLICT
        }
    );
    if maintenance {
        assert_eq!(response.headers()["retry-after"], "1");
    }
    let error = common::json_error(response).await;
    assert_eq!(
        error["code"],
        if maintenance {
            "runtime.busy"
        } else {
            "instance.read_only"
        }
    );
}

async fn assert_queued_heartbeat_refused(fixture: &Fixture, maintenance: bool) {
    let app = router(fixture.state.clone());
    let session = start_playback(fixture, &app).await;
    let before = fixture.session();
    let calls = fixture.clock.calls.load(Ordering::SeqCst);
    let held = HeldWriter::new(fixture.state.store());
    let body = fixture.heartbeat(&session, "progress", 1, 90_000);
    let mut request = Box::pin(app.clone().oneshot(common::request(
        "POST",
        "/api/v1/playback/progress",
        Some(TOKEN),
        body.clone(),
    )));
    // now_ms is called after the heartbeat's pooled read. Once this poll returns
    // Pending, store_mutate/store_write has dispatched the bounded writer work.
    poll_until_write_dispatched(request.as_mut(), || {
        fixture.clock.calls.load(Ordering::SeqCst) > calls
    })
    .await;
    let gate = fixture.state.write_gate();
    let closed = if maintenance {
        Some(gate.close())
    } else {
        let _ = fixture.state.clone().with_instance_writable(false);
        None
    };
    drop(held);
    assert_refusal(request.await.unwrap(), maintenance).await;
    let after = fixture.session();
    assert_eq!(after.last_report_at_ms, before.last_report_at_ms);
    assert_eq!(after.last_event_sequence, before.last_event_sequence);
    assert_eq!(after.position_ms, before.position_ms);
    assert_eq!(after.play_session_id, before.play_session_id);
    drop(closed);
    let _ = fixture.state.clone().with_instance_writable(true);
    // Positive control: the same binding and event really write once admission opens.
    let response = app
        .oneshot(common::request(
            "POST",
            "/api/v1/playback/progress",
            Some(TOKEN),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let after = fixture.session();
    assert_eq!(after.last_event_sequence, Some(1));
    assert_eq!(after.position_ms, 90_000);
    assert!(after.last_report_at_ms > before.last_report_at_ms);
}

/// Axum's ViaParts extractor drops the unused body after middleware admission,
/// before calling this DELETE handler. Its sole await then dispatches the write.
struct ExtractionSignal(Arc<AtomicBool>);

impl Stream for ExtractionSignal {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(None)
    }
}

impl Drop for ExtractionSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn assert_queued_history_delete_refused(fixture: &Fixture, maintenance: bool) {
    let app = router(fixture.state.clone());
    let extracted = Arc::new(AtomicBool::new(false));
    let input = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/search/history/{HISTORY_ID}"))
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::from_stream(ExtractionSignal(extracted.clone())))
        .unwrap();
    let held = HeldWriter::new(fixture.state.store());
    let mut request = Box::pin(app.clone().oneshot(input));
    poll_until_write_dispatched(request.as_mut(), || extracted.load(Ordering::SeqCst)).await;
    let gate = fixture.state.write_gate();
    let closed = if maintenance {
        Some(gate.close())
    } else {
        let _ = fixture.state.clone().with_instance_writable(false);
        None
    };
    drop(held);
    assert_refusal(request.await.unwrap(), maintenance).await;
    {
        let handle = fixture.state.store();
        let store = handle.lock();
        assert_eq!(
            store.get_setting(&fixture.history_key).unwrap().as_deref(),
            Some(fixture.history_value.as_str())
        );
        assert!(store.get_search_snapshot(HISTORY_ID).unwrap().is_some());
    }
    drop(closed);
    let _ = fixture.state.clone().with_instance_writable(true);
    let response = app
        .oneshot(common::request(
            "DELETE",
            &format!("/api/v1/search/history/{HISTORY_ID}"),
            Some(TOKEN),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        fixture
            .state
            .store()
            .lock()
            .get_search_snapshot(HISTORY_ID)
            .unwrap()
            .is_none()
    );
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn queued_heartbeat_is_refused_in_read_only_mode() {
    let fixture = Fixture::new();
    runtime().block_on(assert_queued_heartbeat_refused(&fixture, false));
}

#[test]
fn queued_heartbeat_does_not_land_after_maintenance_starts() {
    let fixture = Fixture::new();
    runtime().block_on(assert_queued_heartbeat_refused(&fixture, true));
}

#[test]
fn queued_history_write_does_not_land_after_maintenance_starts() {
    let fixture = Fixture::new();
    runtime().block_on(assert_queued_history_delete_refused(&fixture, true));
}

#[test]
fn queued_history_write_does_not_land_after_instance_becomes_read_only() {
    let fixture = Fixture::new();
    runtime().block_on(assert_queued_history_delete_refused(&fixture, false));
}
