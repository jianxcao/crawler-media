use api::router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::MediaId;
use serde_json::{Value, json};
use std::path::Path;
use tower::ServiceExt;

use super::common::{add_ledger, json_data, make_state, seed_library, seed_media, seed_member};

fn fixture(root: &Path) -> (axum::Router, MediaId, String, String) {
    let state = make_state(root);
    let store = state.store();
    let library = seed_library(store.as_ref(), root, "stream-credentials");
    let (user, _) = seed_member(store.as_ref(), "stream-viewer");
    // Literal %2B also detects decoding the issued credential more than once.
    let token = "stream+viewer/session==%2B".to_string();
    store.set_user_token(user, &token).unwrap();
    let (_, other_token) = seed_member(store.as_ref(), "another-viewer");
    let media = seed_media(store.as_ref(), "Stream credentials", None);
    let file = add_ledger(store.as_ref(), media, &library, "video");
    let subtitle = library.join("video.en.vtt");
    std::fs::write(
        &subtitle,
        b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n",
    )
    .unwrap();
    store
        .put_file_meta(
            &file.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack {
                    stream_index: Some(0),
                    codec: Some("h264".into()),
                    ..Default::default()
                }),
                audio: Vec::new(),
                subtitles: vec![library::SubtitleTrack {
                    stream_index: Some(2),
                    codec: Some("webvtt".into()),
                    profile: None,
                    language: Some("eng".into()),
                    title: None,
                    bit_rate: None,
                    is_default: true,
                    forced: false,
                    is_external: true,
                    path: Some(subtitle.display().to_string()),
                }],
            },
        )
        .unwrap();
    (router(state), media, token, other_token)
}

async fn issued_session(app: &axum::Router, media: MediaId, token: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::post("/api/v1/playback/sessions")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"media_item_id": media, "device_id": "web-player"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_data(response).await
}

#[tokio::test]
async fn issued_stream_query_credential_is_independent_of_browser_cookie() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, token, other_token) = fixture(tmp.path());
    let session = issued_session(&app, media, &token).await;
    let url = session["stream_url"].as_str().unwrap();
    for cookie in [
        None,
        Some("mc_session=expired-session".to_string()),
        Some(format!("mc_session={other_token}")),
    ] {
        for method in ["GET", "HEAD"] {
            let mut request = Request::builder()
                .method(method)
                .uri(url)
                .header("range", "bytes=0-0");
            if let Some(cookie) = &cookie {
                request = request.header("cookie", cookie);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::PARTIAL_CONTENT,
                "session stream {method}"
            );
            assert_eq!(response.headers()["content-range"], "bytes 0-0/1");
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert_eq!(
                bytes.as_ref(),
                if method == "HEAD" {
                    &b""[..]
                } else {
                    &b"x"[..]
                }
            );
        }
    }
}

#[tokio::test]
async fn stream_uses_explicit_credentials_and_preserves_cookie_only_auth() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, token, other_token) = fixture(tmp.path());
    let session = issued_session(&app, media, &token).await;
    let url = session["stream_url"].as_str().unwrap();
    let cookie = format!("mc_session={token}");
    let cookie_only = url.split_once("&api_key=").unwrap().0;
    let invalid_query = format!("{cookie_only}&api_key=invalid");
    let other_user_query = format!("{cookie_only}&api_key={other_token}");
    let valid_bearer = format!("Bearer {token}");
    for (uri, bearer, expected) in [
        (url, Some("Bearer invalid"), StatusCode::UNAUTHORIZED),
        (
            invalid_query.as_str(),
            Some(valid_bearer.as_str()),
            StatusCode::OK,
        ),
        (invalid_query.as_str(), None, StatusCode::UNAUTHORIZED),
        (other_user_query.as_str(), None, StatusCode::NOT_FOUND),
        (cookie_only, None, StatusCode::OK),
    ] {
        let mut request = Request::get(uri).header("cookie", &cookie);
        if let Some(bearer) = bearer {
            request = request.header("authorization", bearer);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "session stream credentials");
    }
}

#[tokio::test]
async fn issued_subtitle_query_credential_is_decoded_once() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, token, other_token) = fixture(tmp.path());
    let session = issued_session(&app, media, &token).await;
    let urls = session["subtitle_urls"].as_array().unwrap();
    assert_eq!(urls.len(), 1);
    let url = urls[0].as_str().unwrap();
    for cookie in [None, Some(format!("mc_session={other_token}"))] {
        let mut request = Request::get(url);
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "issued subtitle");
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            bytes.as_ref(),
            b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n"
        );
    }
}

#[tokio::test]
async fn query_credential_aliases_remain_case_insensitive() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, token, _) = fixture(tmp.path());
    let session = issued_session(&app, media, &token).await;
    let url = session["stream_url"].as_str().unwrap();
    let (prefix, encoded_token) = url.split_once("&api_key=").unwrap();
    for key in ["API_KEY", "ApiKey", "ToKeN"] {
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("{prefix}&{key}={encoded_token}"))
                    .header("range", "bytes=0-0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::PARTIAL_CONTENT,
            "session stream alias"
        );
    }
}

#[tokio::test]
async fn issued_query_credential_is_rejected_on_nonstream_member_routes() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, token, _) = fixture(tmp.path());
    let session = issued_session(&app, media, &token).await;
    let url = session["stream_url"].as_str().unwrap();
    let query = url.split_once('?').unwrap().1;
    for route in ["/api/v1/libraries", "/api/v1/playback/policy"] {
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("{route}?{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{route}");
    }
}
