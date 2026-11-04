use api::router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::MediaId;
use serde_json::json;
use std::path::Path;
use tower::ServiceExt;

use super::common::{add_ledger, json_data, make_state, seed_library, seed_media, seed_member};

fn fixture(root: &Path) -> (axum::Router, MediaId, String, String) {
    let state = make_state(root);
    let store = state.store();
    let library = seed_library(store.as_ref(), root, "stream-credentials");
    let (_, token) = seed_member(store.as_ref(), "stream-viewer");
    let (_, other_token) = seed_member(store.as_ref(), "another-viewer");
    let media = seed_media(store.as_ref(), "Stream credentials", None);
    add_ledger(store.as_ref(), media, &library, "video");
    (router(state), media, token, other_token)
}

async fn issued_stream(app: &axum::Router, media: MediaId, token: &str) -> String {
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
    json_data(response).await["stream_url"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn issued_stream_query_credential_is_independent_of_browser_cookie() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, token, other_token) = fixture(tmp.path());
    let url = issued_stream(&app, media, &token).await;
    for cookie in [
        String::new(),
        "mc_session=expired-session".into(),
        format!("mc_session={other_token}"),
    ] {
        for method in ["GET", "HEAD"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(&url)
                        .header("cookie", &cookie)
                        .header("range", "bytes=0-0")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
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
    let url = issued_stream(&app, media, &token).await;
    let cookie = format!("mc_session={token}");
    let invalid_query = url.replace(&format!("api_key={token}"), "api_key=invalid");
    let other_user_query = url.replace(
        &format!("api_key={token}"),
        &format!("api_key={other_token}"),
    );
    let cookie_only = url.split("&api_key=").next().unwrap();
    for (uri, bearer, expected) in [
        (
            url.as_str(),
            Some("Bearer invalid"),
            StatusCode::UNAUTHORIZED,
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
        assert_eq!(response.status(), expected);
    }
    let response = app
        .oneshot(
            Request::get(format!("/api/v1/libraries?api_key={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
