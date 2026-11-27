//! P1-4 & P2-6: Jellyfin camelCase container stream params (`mediaSourceId`, `playSessionId`,
//! `deviceId`) and percent-escaped query `DeviceId` binding across PlaybackInfo, streaming,
//! progress reporting, session closure, and device revocation.

use std::path::Path;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use serde_json::Value;
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

struct Fixture {
    app: axum::Router,
    item_id: String,
    media_id: MediaId,
}

fn fixture(root: &Path) -> Fixture {
    let store = Store::open(root.join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Bound".into(),
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
    let dir = root.join("data/library/movies");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bound.mkv");
    std::fs::write(&path, b"AAAAAAAA").unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id: media.id,
            path: path.display().to_string(),
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            ..Default::default()
        },
    )
    .unwrap();
    let app = router(
        ApiState::new(
            store,
            "admin-token".into(),
            RegistrySnapshot::builtin().unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(root.join("stage"))),
        )
        .unwrap(),
    );
    Fixture {
        app,
        item_id: domain::TypedItemId::Work(media.id).to_wire_string(),
        media_id: media.id,
    }
}

fn publish_new_preferred_source(root: &Path, media_id: MediaId) -> String {
    let store = Store::open(root.join("data")).unwrap();
    let other_path = root.join("data/library/movies/preferred.mkv");
    std::fs::write(&other_path, b"BBBBBBBB").unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id,
            path: other_path.display().to_string(),
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            ..Default::default()
        },
    )
    .unwrap();
    let scope = store::library_write::WriteScope {
        library_id: file.library_id,
        config_revision: store
            .library_config_revision(&file.library_id.to_string())
            .unwrap(),
    };
    std::fs::write(&other_path, b"BBBBBBBB revised").unwrap();
    let observation = api::fingerprint_job::observe_source(&other_path).unwrap();
    store
        .replace_file_source(&scope, file.file_id, &file.version, &observation, 5_000)
        .unwrap();
    let user = store.list_users().unwrap()[0].id;
    let entries = store
        .playable_entries(user, &domain::TypedItemId::Work(media_id))
        .unwrap();
    assert_eq!(entries[0].file.file_id, file.file_id);
    domain::PhysicalSourceId {
        file_id: file.file_id,
        revision: entries[0].file.version.revision,
    }
    .to_wire_string()
}

async fn send(app: &axum::Router, method: &str, uri: &str, device: &str) -> (StatusCode, Vec<u8>) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer admin-token")
        .header("X-Emby-Device-Id", device)
        .header("X-Emby-Client", "Swiftfin")
        .header("content-type", "application/json")
        .header("range", "bytes=0-3")
        .body(Body::from("{}"))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, body)
}

#[tokio::test]
async fn p1_4_camel_case_container_stream_preserves_session_source_and_closing_invalidates() {
    for (prefix, container) in [("", "mkv"), ("/emby", "mp4")] {
        let tmp = tempfile::tempdir().unwrap();
        let f = fixture(tmp.path());
        let (status, body) = send(
            &f.app,
            "POST",
            &format!("{prefix}/Items/{}/PlaybackInfo", f.item_id),
            "swift-1",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let info: Value = serde_json::from_slice(&body).unwrap();
        let sid = info["PlaySessionId"].as_str().unwrap().to_string();
        let source_a = info["MediaSources"][0]["Id"].as_str().unwrap().to_string();

        let _source_b = publish_new_preferred_source(tmp.path(), f.media_id);

        let uri = format!(
            "{prefix}/Videos/{}/stream.{container}?mediaSourceId={source_a}&playSessionId={sid}&deviceId=swift-1",
            f.item_id
        );
        let (status, bytes) = send(&f.app, "GET", &uri, "swift-1").await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT, "{uri}");
        assert_eq!(bytes, b"AAAA", "existing session must keep source A bytes");

        let uri_no_src = format!(
            "{prefix}/Videos/{}/stream.{container}?playSessionId={sid}&deviceId=swift-1",
            f.item_id
        );
        let (status, bytes) = send(&f.app, "GET", &uri_no_src, "swift-1").await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT, "{uri_no_src}");
        assert_eq!(bytes, b"AAAA");

        for endpoint in ["Sessions/Playing", "Sessions/Playing/Stopped"] {
            let event_req = Request::builder()
                .method("POST")
                .uri(format!("{prefix}/{endpoint}"))
                .header("authorization", "Bearer admin-token")
                .header("X-Emby-Device-Id", "swift-1")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "ItemId": f.item_id,
                        "PositionTicks": 10_000_000,
                        "PlaySessionId": sid,
                        "MediaSourceId": source_a,
                    })
                    .to_string(),
                ))
                .unwrap();
            let event_res = f.app.clone().oneshot(event_req).await.unwrap();
            assert_eq!(event_res.status(), StatusCode::NO_CONTENT);
        }

        let (status, _) = send(&f.app, "GET", &uri, "swift-1").await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "closed session must be rejected on {uri}"
        );
    }
}

#[tokio::test]
async fn p2_6_escaped_query_device_id_binds_across_playback_stream_and_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let f = fixture(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = store.list_users().unwrap()[0].id;
    let user_token = "living-room-user-token";
    store.set_user_token(user, user_token).unwrap();
    let encoded = "Living%20Room%20TV";

    let info_req = Request::builder()
        .method("POST")
        .uri(format!(
            "/Items/{}/PlaybackInfo?api_key={user_token}&DeviceId={encoded}",
            f.item_id
        ))
        .body(Body::empty())
        .unwrap();
    let info_res = f.app.clone().oneshot(info_req).await.unwrap();
    assert_eq!(info_res.status(), StatusCode::OK);
    let info_body = to_bytes(info_res.into_body(), usize::MAX).await.unwrap();
    let info: Value = serde_json::from_slice(&info_body).unwrap();
    let sid = info["PlaySessionId"].as_str().unwrap().to_string();
    let source_a = info["MediaSources"][0]["Id"].as_str().unwrap().to_string();

    let stream_req = Request::builder()
        .method("GET")
        .uri(format!(
            "/Videos/{}/stream.mkv?api_key={user_token}&mediaSourceId={source_a}&playSessionId={sid}&deviceId={encoded}",
            f.item_id
        ))
        .header("range", "bytes=0-3")
        .body(Body::empty())
        .unwrap();
    let stream_res = f.app.clone().oneshot(stream_req).await.unwrap();
    assert_eq!(stream_res.status(), StatusCode::PARTIAL_CONTENT);
    let bytes = to_bytes(stream_res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(bytes.as_ref(), b"AAAA");

    let prog_req = Request::builder()
        .method("POST")
        .uri(format!(
            "/Sessions/Playing?api_key={user_token}&DeviceId={encoded}"
        ))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "ItemId": f.item_id,
                "PositionTicks": 50_000_000,
                "PlaySessionId": sid,
                "MediaSourceId": source_a,
            })
            .to_string(),
        ))
        .unwrap();
    let prog_res = f.app.clone().oneshot(prog_req).await.unwrap();
    assert_eq!(prog_res.status(), StatusCode::NO_CONTENT);

    store.revoke_device(user, "Living Room TV").unwrap();
    let old_token_req = Request::builder()
        .method("GET")
        .uri(format!(
            "/Videos/{}/stream.mkv?api_key={user_token}&mediaSourceId={source_a}&playSessionId={sid}&deviceId={encoded}",
            f.item_id
        ))
        .header("range", "bytes=0-3")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        f.app.clone().oneshot(old_token_req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    let fresh_token = "living-room-user-token-2";
    store.set_user_token(user, fresh_token).unwrap();
    let blocked_req = Request::builder()
        .method("GET")
        .uri(format!(
            "/Videos/{}/stream.mkv?api_key={fresh_token}&mediaSourceId={source_a}&playSessionId={sid}&deviceId={encoded}",
            f.item_id
        ))
        .header("range", "bytes=0-3")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        f.app.clone().oneshot(blocked_req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}
