use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn legacy_management_paths_are_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    for token in [Some("management-secret"), None] {
        for uri in [
            "/subscribes",
            "/filters",
            "/search",
            "/users",
            "/sites",
            "/downloaders",
            "/jobs",
            "/ledger",
            "/unidentified",
            "/catalog/search",
            "/downloads",
            "/directory",
        ] {
            let response = app
                .clone()
                .oneshot(request("GET", uri, token, Value::Null))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "GET {uri} (token={token:?}) should be 404"
            );
        }
    }

    // POST /subscribes/{id}/run on legacy root must be 404
    for token in [Some("management-secret"), None] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/subscribes/test-id/run",
                token,
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "POST /subscribes/test-id/run should be 404"
        );
    }
}

#[tokio::test]
async fn jellyfin_vs_legacy_user_routes_case_sensitivity() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));

    // Lowercase /users (legacy management) is 404
    let lower = app
        .clone()
        .oneshot(request("GET", "/users", None, Value::Null))
        .await
        .unwrap();
    assert_eq!(
        lower.status(),
        StatusCode::NOT_FOUND,
        "lowercase /users must 404"
    );

    // Uppercase /Users/Me is Jellyfin protocol (unauthenticated -> 401 Unauthorized, NOT 404)
    let jellyfin_user = app
        .oneshot(request("GET", "/Users/Me", None, Value::Null))
        .await
        .unwrap();
    assert_eq!(
        jellyfin_user.status(),
        StatusCode::UNAUTHORIZED,
        "uppercase Jellyfin /Users/Me must reach Jellyfin auth handler (401), not 404"
    );
}

#[tokio::test]
async fn jellyfin_public_ping_still_works() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let response = app
        .oneshot(request("GET", "/System/Ping", None, Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
