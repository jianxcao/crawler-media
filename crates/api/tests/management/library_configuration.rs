use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

async fn get(app: &axum::Router, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(request("GET", uri, Some("management-secret"), Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

/// 轮询媒体库操作到终态。
///
/// 带 `root_paths`/`display_locale` 的 PATCH 按异步契约受理（`202` + 操作视图，
/// 见 `crates/api/src/http/library_config.rs:166-214`），响应体不再是 Library DTO，
/// 因此业务断言必须在操作终态之后从 `GET /libraries` 读回。
async fn poll_library_operation(app: &axum::Router, library_id: &str, operation_id: &str) -> Value {
    let uri = format!("/api/v1/libraries/{library_id}/operations/{operation_id}");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let body = get(app, &uri).await;
            let state = body["data"]["state"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if state == "succeeded" {
                break body;
            }
            assert_ne!(state, "failed", "媒体库操作失败: {body}");
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("媒体库操作必须到达终态")
}

#[tokio::test]
async fn libraries_seed_default_movie_and_tv() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let listed = get(&app, "/api/v1/libraries").await;
    let rows = listed["data"].as_array().expect("array of libraries");
    assert_eq!(rows.len(), 2);

    let movie = rows.iter().find(|l| l["kind"] == "movie").unwrap();
    assert_eq!(movie["name"], "电影库");
    assert_eq!(movie["is_default"], true);

    let tv = rows.iter().find(|l| l["kind"] == "tv").unwrap();
    assert_eq!(tv["name"], "剧集库");
    assert_eq!(tv["is_default"], true);
}

#[tokio::test]
async fn libraries_create_with_two_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "华语电影",
                    "kind": "movie",
                    "root_paths": ["/data/movies/cn1", "/data/movies/cn2"],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(created["data"]["name"], "华语电影");
    assert_eq!(created["data"]["kind"], "movie");
    assert_eq!(created["data"]["is_default"], false);
    let roots = created["data"]["root_paths"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0], "/data/movies/cn1");
    assert_eq!(roots[1], "/data/movies/cn2");
}

#[tokio::test]
async fn libraries_reject_unknown_kind_and_empty_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/libraries",
            Some("management-secret"),
            json!({
                "name": "非法",
                "kind": "music",
                "root_paths": ["/data/music"],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/libraries",
            Some("management-secret"),
            json!({
                "name": "无根路径",
                "kind": "movie",
                "root_paths": [],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn libraries_patch_replaces_roots_and_renames() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "欧美剧",
                    "kind": "tv",
                    "root_paths": ["/data/tv/us"],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap();

    let patched = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/libraries/{id}"),
            Some("management-secret"),
            json!({
                "name": "欧美精选剧集",
                "root_paths": ["/data/tv/us_archive", "/data/tv/us_current"],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        patched.status(),
        StatusCode::ACCEPTED,
        "换根要重写台账与 NFO 归属，按异步操作受理"
    );
    let accepted = json_body(patched).await;
    let operation_id = accepted["data"]["operation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("202 必须带 operation_id: {accepted}"))
        .to_string();
    assert_eq!(accepted["data"]["kind"], "roots");
    let finished = poll_library_operation(&app, id, &operation_id).await;
    assert_eq!(finished["data"]["state"], "succeeded");

    // 操作到达终态后读回：改名与换根都必须真实落库，且不得多出 library 行。
    let listed = get(&app, "/api/v1/libraries").await;
    let rows = listed["data"].as_array().unwrap();
    let matched: Vec<&Value> = rows.iter().filter(|library| library["id"] == id).collect();
    assert_eq!(matched.len(), 1, "改名/换根不得产生重复库行: {listed}");
    assert_eq!(matched[0]["name"], "欧美精选剧集");
    let roots = matched[0]["root_paths"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0], "/data/tv/us_archive");
    assert_eq!(roots[1], "/data/tv/us_current");
}
