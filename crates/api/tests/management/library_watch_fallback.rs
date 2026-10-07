//! 「未看优先」的回退（`w_fallback`）：`/libraries/{id}/items` 上的那条口径。
//!
//! 首页库行的语义是"最近入库、未观看优先；这个库全看过了就退回全部"——整行消失
//! 比"多看到几部已经看过的"更糟。墙上用户手选的「未观看」不带这个开关，是严格筛，
//! 两者必须分辨得出来。

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

/// 建一个有两部电影的电影库并扫描，返回 (library id, 两部条目的 media id)。
async fn seed(tmp: &tempfile::TempDir, app: &axum::Router) -> (String, Vec<String>) {
    let movies = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movies).unwrap();
    std::fs::write(movies.join("The.Matrix.1999.2160p.mkv"), b"m").unwrap();
    std::fs::write(movies.join("Dune.2021.1080p.mkv"), b"d").unwrap();
    let libraries = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/libraries",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let library_id = libraries
        .as_array()
        .unwrap()
        .iter()
        .find(|library| library["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let scan = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{library_id}/scan"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(scan.status(), StatusCode::OK);
    let items = list_items(app, &library_id, "").await;
    let ids: Vec<String> = items
        .iter()
        .map(|item| item["media_item_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 2, "扫描后应有两部电影：{items:?}");
    (library_id, ids)
}

/// 按首页库行的口径取条目；`extra` 是附加的查询串（`&w=...` 之类）。
async fn list_items(app: &axum::Router, library_id: &str, extra: &str) -> Vec<Value> {
    let body = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{library_id}/items?sort=added_at&limit=50{extra}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    body.as_array().cloned().unwrap_or_default()
}

async fn mark_played(app: &axum::Router, media_item_id: &str) {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/marks",
            Some("management-secret"),
            json!({ "media_item_id": media_item_id, "played": true }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn watch_fallback_covers_a_fully_watched_library() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (library_id, ids) = seed(&tmp, &app).await;
    for id in &ids {
        mark_played(&app, id).await;
    }

    // 严格筛：就是空的——墙上用户手选的「未观看」要的正是这个
    assert!(list_items(&app, &library_id, "&w=unwatched").await.is_empty());

    // 未看优先：退回全部，首页那一行不会整段消失
    let fallback = list_items(&app, &library_id, "&w=unwatched&w_fallback=true").await;
    assert_eq!(fallback.len(), 2);
}

#[tokio::test]
async fn watch_fallback_only_kicks_in_when_nothing_is_unwatched() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (library_id, ids) = seed(&tmp, &app).await;
    mark_played(&app, &ids[0]).await;

    let strict = list_items(&app, &library_id, "&w=unwatched").await;
    assert_eq!(strict.len(), 1, "还有没看过的：严格筛就该只剩它");
    let with_fallback = list_items(&app, &library_id, "&w=unwatched&w_fallback=true").await;
    assert_eq!(with_fallback.len(), 1, "有没看过的就不回退，两份名单一致");
    assert_eq!(with_fallback[0]["media_item_id"], strict[0]["media_item_id"]);
}

#[tokio::test]
async fn watch_fallback_is_rejected_when_not_a_bool() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (library_id, _) = seed(&tmp, &app).await;
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{library_id}/items?w=unwatched&w_fallback=maybe"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
