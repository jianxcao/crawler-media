use store::test_support::{FixtureFile, publish_fixture_file};
#[path = "support/mod.rs"]
mod support;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn app(root: &Path) -> (axum::Router, MediaId, String) {
    let store = Store::open(root.join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let library_root = root.join("data/library/movies");
    std::fs::create_dir_all(&library_root).unwrap();
    let file = library_root.join("matrix.mkv");
    std::fs::write(&file, b"0123456789abcdef").unwrap();
    let spec = FixtureFile {
        file_id: Some(LedgerId::new()),
        media_id: media.id,
        path: file.display().to_string(),
        season: None,
        episode: None,
        resolution: Some("2160p".into()),
        codec: Some("hevc".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
        ..Default::default()
    };
    publish_fixture_file(&store, &spec).unwrap();
    let item_id = domain::TypedItemId::Work(media.id).to_wire_string();
    let router = router(
        ApiState::new(
            store,
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(root.join("stage"))),
            root.join("library"),
        )
        .unwrap(),
    );
    (router, media.id, item_id)
}

async fn json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn admin() -> domain::UserId {
    domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap()
}

/// The published file behind the fixture movie.
fn file_id(store: &Store, media: MediaId) -> domain::LedgerId {
    store
        .file_coverage_for_media(media)
        .unwrap()
        .remove(0)
        .file
        .file_id
}

/// Issue the play session that protocol events must carry.
fn issue_session(store: &Store, device: &str, media: MediaId) -> String {
    support::session::issue_session(store, admin(), device, file_id(store, media), 1)
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    auth: &str,
    body: Value,
    device: Option<&str>,
) -> axum::response::Response {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", auth)
        .header("content-type", "application/json");
    if let Some(id) = device {
        b = b
            .header("X-Emby-Device-Id", id)
            .header("X-Emby-Device-Name", "Bedroom Apple TV")
            .header("X-Emby-Client", "Infuse");
    }
    app.clone()
        .oneshot(b.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}
#[tokio::test]
async fn jellyfin_completion_marks_watched() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let session = issue_session(&store, "actual-device", media);
    store
        .upsert_unit(
            admin(),
            media,
            -1,
            -1,
            0,
            None,
            None,
            Some(120000),
            None,
            None,
            false,
            1,
        )
        .unwrap();
    for (route, ticks) in [
        ("/Sessions/Playing", 0),
        ("/Sessions/Playing/Progress", 1200000000),
        ("/Sessions/Playing/Stopped", 1200000000),
    ] {
        assert_eq!(
            send(
                &app,
                "POST",
                route,
                "Bearer admin-token",
                json!({"ItemId":id,"PositionTicks":ticks,"PlaySessionId":session}),
                Some("actual-device")
            )
            .await
            .status(),
            StatusCode::NO_CONTENT
        );
    }
    let state = store.unit_state(admin(), media, -1, -1).unwrap().unwrap();
    assert!(state.played);
    assert_eq!(state.position_ms, 120000);
    let result = json(
        send(
            &app,
            "GET",
            "/Users/Me/Items/Resume",
            "Bearer admin-token",
            json!({}),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(result["TotalRecordCount"], 0);
}
/// A protocol Progress without a preceding Playing cannot invent a binding.
#[tokio::test]
async fn unbound_jellyfin_progress_is_ignored_without_fabricating_facts() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();

    let response = send(
        &app,
        "POST",
        "/Sessions/Playing/Progress",
        "Bearer admin-token",
        json!({"ItemId":id,"PositionTicks":50000000}),
        Some("unbound-device"),
    )
    .await;
    let status = response.status();
    if status != StatusCode::NO_CONTENT {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        panic!(
            "未绑定 Progress 应答 {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }

    assert!(store.unit_state(admin(), media, -1, -1).unwrap().is_none());

    assert!(
        store
            .get_session(admin(), "unbound-device")
            .unwrap()
            .is_none(),
        "未绑定 Progress 不得伪造 live 会话行"
    );
}

#[tokio::test]
async fn standard_authorization_isolates_devices() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    for device in ["bedroom", "living-room"] {
        let session = issue_session(&store, device, media);
        let auth =
            format!("MediaBrowser Token=\"admin-token\", DeviceId=\"{device}\", Client=\"Infuse\"");
        assert_eq!(
            send(
                &app,
                "POST",
                "/Sessions/Playing",
                &auth,
                json!({"ItemId":id,"PositionTicks":0,"PlaySessionId":session}),
                None
            )
            .await
            .status(),
            StatusCode::NO_CONTENT
        );
    }
    let sessions = store.active_sessions(i64::MAX / 2, i64::MAX / 2).unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        store
            .get_session(admin(), "bedroom")
            .unwrap()
            .unwrap()
            .client
            .as_deref(),
        Some("Infuse")
    );
    assert!(store.get_session(admin(), "living-room").unwrap().is_some());
}
#[tokio::test]
async fn stopped_device_retains_revocable_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    store.set_user_token(admin(), "device-token").unwrap();
    let session = issue_session(&store, "actual-device", media);
    for route in ["/Sessions/Playing", "/Sessions/Playing/Stopped"] {
        assert_eq!(
            send(
                &app,
                "POST",
                route,
                "Bearer device-token",
                json!({"ItemId":id,"PositionTicks":10000000,"PlaySessionId":session}),
                Some("actual-device")
            )
            .await
            .status(),
            StatusCode::NO_CONTENT
        );
    }
    let body = json(
        send(
            &app,
            "GET",
            "/api/v1/playback/devices",
            "Bearer admin-token",
            json!({}),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(body["data"][0]["device_id"], "actual-device");
    let revoke = format!("/api/v1/playback/devices/actual-device?user_id={}", admin());
    assert_eq!(
        send(
            &app,
            "DELETE",
            &revoke,
            "Bearer admin-token",
            json!({}),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    let stream = format!("/Videos/{id}/stream");
    assert_eq!(
        send(
            &app,
            "GET",
            &stream,
            "Bearer device-token",
            json!({}),
            Some("actual-device")
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(
            &app,
            "GET",
            &stream,
            "Bearer admin-token",
            json!({}),
            Some("other-device")
        )
        .await
        .status(),
        StatusCode::OK
    );
}
#[tokio::test]
async fn unit_write_failure_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let session = issue_session(&store, "browser", media);
    let db = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
    db.execute_batch("CREATE TRIGGER review_access_tmp_block BEFORE INSERT ON playback_units BEGIN SELECT RAISE(ABORT,'review write blocked'); END;").unwrap();
    let response=send(&app,"POST","/api/v1/playback/progress","Bearer admin-token",json!({ "event_sequence": crate::support::session::next_sequence(),"media_item_id":media.to_string(),"event":"start","device_id":"browser","position_ms":30000,"play_session_id":session}),None).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(json(response).await["data"].is_null());
    assert!(store.unit_state(admin(), media, -1, -1).unwrap().is_none());
}
#[tokio::test]
async fn zero_playback_limits_return_a_bounded_page() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, _) = app(tmp.path());
    send(
        &app,
        "POST",
        "/api/v1/playback/marks",
        "Bearer admin-token",
        json!({"media_item_id": media.to_string(), "favorite": true}),
        None,
    )
    .await;
    for route in [
        "/api/v1/playback/favorites?limit=0",
        "/api/v1/playback/favorites/gallery?limit=0",
    ] {
        let response = send(&app, "GET", route, "Bearer admin-token", json!({}), None).await;
        assert_eq!(response.status(), StatusCode::OK, "{route}");
        let body = json(response).await;
        let items = body["data"]["items"]
            .as_array()
            .or_else(|| body["data"].as_array())
            .unwrap_or_else(|| panic!("{route} missing bounded collection: {body}"));
        assert_eq!(
            items.len(),
            1,
            "{route} did not clamp limit=0 to one existing item"
        );
    }
}

#[tokio::test]
async fn zero_history_limit_is_safe() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, id) = app(tmp.path());
    for route in ["/Sessions/Playing", "/Sessions/Playing/Stopped"] {
        send(
            &app,
            "POST",
            route,
            "Bearer admin-token",
            json!({"ItemId":id,"PositionTicks":10000000}),
            Some("actual-device"),
        )
        .await;
    }
    let handle = tokio::spawn(async move {
        send(
            &app,
            "GET",
            "/api/v1/playback/history?limit=0",
            "Bearer admin-token",
            json!({}),
            None,
        )
        .await
    });
    assert_eq!(handle.await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn web_revoke_is_explicitly_unsupported() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let session = issue_session(&store, "browser", media);
    let body = json!({ "event_sequence": crate::support::session::next_sequence(),"media_item_id":media.to_string(),"event":"start","device_id":"browser","position_ms":30000,"play_session_id":session});
    assert_eq!(
        send(
            &app,
            "POST",
            "/api/v1/playback/progress",
            "Bearer admin-token",
            body.clone(),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    let revoke = format!("/api/v1/playback/devices/browser?user_id={}", admin());
    let response = send(
        &app,
        "DELETE",
        &revoke,
        "Bearer admin-token",
        json!({}),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(response).await["error"]["code"],
        "playback.unsupported"
    );
    assert_eq!(
        send(
            &app,
            "POST",
            "/api/v1/playback/progress",
            "Bearer admin-token",
            body,
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        send(
            &app,
            "GET",
            &format!("/Videos/{id}/stream?api_key=admin-token"),
            "Bearer admin-token",
            json!({}),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
}
#[tokio::test]
async fn frontend_metrics_persist() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let file = file_id(&store, media);
    let response = send(
        &app,
        "POST",
        "/api/v1/playback/metrics",
        "Bearer admin-token",
        json!({"library_file_id":file.to_string(),"tier":0,"engine":"native","watched_ms":60000}),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let db = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
    let row = db
        .query_row(
            "SELECT user_id,media_id,engine,watched_ms FROM playback_metrics",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            admin().to_string(),
            media.to_string(),
            "native".into(),
            60000
        )
    );
}
#[tokio::test]
async fn clear_history_preserves_favorites_and_preferences() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .upsert_unit(
            admin(),
            media,
            -1,
            -1,
            1000,
            Some(true),
            Some(true),
            Some(120000),
            Some("embedded:1"),
            Some("off"),
            true,
            1,
        )
        .unwrap();
    assert_eq!(
        send(
            &app,
            "DELETE",
            "/api/v1/playback/history?scope=all",
            "Bearer admin-token",
            json!({}),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    let body = json(
        send(
            &app,
            "GET",
            "/api/v1/playback/favorites",
            "Bearer admin-token",
            json!({}),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(body["data"]["total"], 1);
    let state = store.unit_state(admin(), media, -1, -1).unwrap().unwrap();
    assert_eq!(
        (state.position_ms, state.played, state.play_count),
        (0, false, 0)
    );
    // The favourite is a library decision and survives; the resume package (position,
    // duration, source binding and the track selection bound to it) goes with the history.
    assert!(state.favorite, "the favourite survives clearing history");
    assert_eq!(
        (
            state.audio_track.as_deref(),
            state.subtitle_track.as_deref(),
            state.duration_ms
        ),
        (None, None, None),
        "the resume package is cleared with the history"
    );
    assert!(store.user_active_media_ids(admin()).unwrap().is_empty());
}

#[path = "playback_regression/credentials.rs"]
mod credentials;
#[path = "playback_regression/failures.rs"]
mod failures;
#[path = "playback_regression/protocol.rs"]
mod protocol;
