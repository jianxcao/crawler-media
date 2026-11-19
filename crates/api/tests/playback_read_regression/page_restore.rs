use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

use api::{playback_events::PlaybackClock, router};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

struct Clock(AtomicI64);
impl api::playback_events::PlaybackClock for Clock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    state: api::ApiState,
    app: axum::Router,
    clock: Arc<Clock>,
    user: domain::UserId,
    token: String,
    media: domain::MediaId,
}
impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let clock = Arc::new(Clock(AtomicI64::new(now_secs() * 1000)));
        let state = make_state(tmp.path()).with_playback_clock(clock.clone());
        let store = state.store();
        let root = seed_library(&store, tmp.path(), "restore");
        let (user, token) = seed_member(&store, "restore-user");
        let media = seed_media(&store, "Restorable", Some(2025));
        add_ledger(&store, media, &root, "restorable");
        let app = router(state.clone());
        Self {
            _tmp: tmp,
            state,
            app,
            clock,
            user,
            token,
            media,
        }
    }
    async fn post(&self, endpoint: &str, body: Value) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/playback/{endpoint}"))
                    .header("authorization", format!("Bearer {}", self.token))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }
    async fn issue(&self, previous: Option<&str>) -> Value {
        let mut body =
            json!({"media_item_id":self.media,"device_id":"restore-device","start_ms":42_000});
        if let Some(previous) = previous {
            body["resume_from_play_session_id"] = json!(previous);
        }
        json_data(self.post("sessions", body).await).await
    }
    async fn event(&self, sid: &str, event: &str, sequence: i64, paused: bool) {
        json_data(
            self.post(
                "progress",
                json!({
                    "media_item_id":self.media,"device_id":"restore-device",
                    "play_session_id":sid,"event":event,"event_sequence":sequence,
                    "position_ms":42_000,"paused":paused,
                }),
            )
            .await,
        )
        .await;
    }
    async fn assert_range(&self, session: &Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(session["stream_url"].as_str().unwrap())
                    .header("range", "bytes=0-0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(&to_bytes(response.into_body(), 10).await.unwrap()[..], b"x");
    }
}

async fn error(response: axum::response::Response, status: StatusCode, code: &str) {
    let actual_status = response.status();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(actual_status, status, "{body}");
    assert_eq!(body["error"]["code"], code, "{body}");
}

#[tokio::test]
async fn page_restore_rejects_malformed_predecessor_without_issuing_a_generation() {
    let f = Fixture::new();
    for previous in [
        json!(""),
        json!("not-a-uuid"),
        json!(42),
        Value::Null,
        json!({}),
    ] {
        error(
            f.post(
                "sessions",
                json!({
                    "media_item_id":f.media,"device_id":"restore-device",
                    "resume_from_play_session_id":previous,
                }),
            )
            .await,
            StatusCode::BAD_REQUEST,
            "playback.invalid",
        )
        .await;
    }
    let issued = f.issue(None).await;
    let binding = f
        .state
        .store()
        .play_session_binding(
            f.user,
            "restore-device",
            issued["play_session_id"].as_str().unwrap(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(binding.generation, 1);
}

#[tokio::test]
async fn page_restore_renews_paused_and_idle_expired_sessions_with_readable_new_urls() {
    let f = Fixture::new();
    let original = f.issue(None).await;
    let sid = original["play_session_id"].as_str().unwrap();
    f.event(sid, "start", 1, false).await;
    f.event(sid, "progress", 2, true).await;
    f.assert_range(&original).await;
    let live = f
        .state
        .store()
        .get_session(f.user, "restore-device")
        .unwrap()
        .unwrap();
    assert!(live.paused);
    assert_eq!(live.watched_ms, 0);
    let next = f.issue(Some(sid)).await;
    assert_ne!(next["play_session_id"], original["play_session_id"]);
    assert_eq!(next["start_ms"], 42_000);
    f.assert_range(&next).await;
    let next_sid = next["play_session_id"].as_str().unwrap();
    f.event(next_sid, "start", 1, false).await;
    f.clock.0.fetch_add(301_000, Ordering::SeqCst);
    assert_eq!(
        f.state
            .store()
            .sweep_stale_sessions(f.clock.now_ms() / 1000, 300)
            .unwrap(),
        1
    );
    let renewed = f.issue(Some(next_sid)).await;
    f.assert_range(&renewed).await;
    let store = f.state.store();
    let first = store
        .play_session_binding(f.user, "restore-device", sid)
        .unwrap()
        .unwrap();
    let last = store
        .play_session_binding(
            f.user,
            "restore-device",
            renewed["play_session_id"].as_str().unwrap(),
        )
        .unwrap()
        .unwrap();
    assert!(last.generation > first.generation);
}

#[tokio::test]
async fn page_restore_refuses_admin_ended_live_and_archived_predecessors() {
    let f = Fixture::new();
    let original = f.issue(None).await;
    let sid = original["play_session_id"].as_str().unwrap();
    f.event(sid, "start", 1, false).await;
    let now = f.clock.0.load(Ordering::SeqCst) / 1000;
    json_data(
        f.post("activity/sessions/restore-device/end", json!({}))
            .await,
    )
    .await;
    for archive in [false, true] {
        if archive {
            f.state
                .store()
                .close_session(f.user, "restore-device", now + 1)
                .unwrap();
            f.clock.0.fetch_add(20_000, Ordering::SeqCst);
        }
        error(
            f.post(
                "sessions",
                json!({
                    "media_item_id":f.media,"device_id":"restore-device",
                    "resume_from_play_session_id":sid,
                }),
            )
            .await,
            StatusCode::CONFLICT,
            "playback.ended_by_admin",
        )
        .await;
    }
}

#[tokio::test]
async fn page_restore_authorizes_exact_user_device_target_and_latest_generation() {
    let f = Fixture::new();
    let original = f.issue(None).await;
    let sid = original["play_session_id"].as_str().unwrap();
    error(f.post("sessions", json!({
        "media_item_id":f.media,"device_id":"other-device","resume_from_play_session_id":sid,
    })).await, StatusCode::NOT_FOUND, "playback.session_missing").await;
    let (other, _) = seed_member(&f.state.store(), "other-restore-user");
    let file = f
        .state
        .store()
        .resolve_playback(other, &domain::TypedItemId::Work(f.media), None, None)
        .unwrap()
        .target
        .file;
    let foreign = f
        .state
        .store()
        .issue_play_session(
            other,
            "restore-device",
            file.file_id,
            f.media,
            domain::UnitKey::WHOLE,
            &file.version,
            now_secs(),
        )
        .unwrap();
    error(f.post("sessions", json!({
        "media_item_id":f.media,"device_id":"restore-device","resume_from_play_session_id":foreign,
    })).await, StatusCode::NOT_FOUND, "playback.session_missing").await;
    let store = f.state.store();
    let root = seed_library(&store, f._tmp.path(), "other");
    let media = seed_media(&store, "Other target", None);
    add_ledger(&store, media, &root, "other");
    error(f.post("sessions", json!({
        "media_item_id":media,"device_id":"restore-device","resume_from_play_session_id":sid,
    })).await, StatusCode::CONFLICT, "playback.resume_target_changed").await;
    f.issue(None).await;
    error(f.post("sessions", json!({
        "media_item_id":f.media,"device_id":"restore-device","resume_from_play_session_id":sid,
    })).await, StatusCode::CONFLICT, "playback.resume_superseded").await;
}

#[tokio::test]
async fn page_restore_uses_current_source_and_rechecks_library_access() {
    let f = Fixture::new();
    let original = f.issue(None).await;
    let sid = original["play_session_id"].as_str().unwrap();
    f.event(sid, "start", 1, false).await;
    f.event(sid, "progress", 2, true).await;
    let store = f.state.store();
    let resolved = store
        .resolve_playback(f.user, &domain::TypedItemId::Work(f.media), None, None)
        .unwrap();
    let mut replacement = resolved.target.file.clone();
    replacement.version.revision += 1;
    replacement.version.source_version = "replacement-source".into();
    store
        .publish_files(
            &store::library_write::WriteScope {
                library_id: replacement.library_id,
                config_revision: resolved.target.config_revision,
            },
            &[store::library_write::FilePublication {
                file: &replacement,
                units: &[domain::UnitKey::WHOLE],
                expected_version: Some(&resolved.target.file.version),
                nfo_observation: None,
                probe_required: false,
            }],
            f.clock.now_ms(),
        )
        .unwrap();
    let next = f.issue(Some(sid)).await;
    assert_eq!(next["resume_status"], "source_changed");
    assert_eq!(next["watch"]["audio_track"], Value::Null);
    f.assert_range(&next).await;
    let next_sid = next["play_session_id"].as_str().unwrap();
    let binding = store
        .play_session_binding(f.user, "restore-device", next_sid)
        .unwrap()
        .unwrap();
    assert_eq!(binding.file_revision, Some(replacement.version.revision));
    assert_eq!(
        binding.source_version.as_deref(),
        Some("replacement-source")
    );
    store
        .update_library(
            &replacement.library_id.to_string(),
            None,
            None,
            Some(("selected", true, &[])),
        )
        .unwrap();
    error(f.post("sessions", json!({
        "media_item_id":f.media,"device_id":"restore-device","resume_from_play_session_id":next_sid,
    })).await, StatusCode::NOT_FOUND, "playback.item_missing").await;
}
