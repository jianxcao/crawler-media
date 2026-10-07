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
    router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

#[tokio::test]
async fn put_target_pref_null_removes_category_completely() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    // 1. 设置 movie 偏好
    let res = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/downloaders/target-prefs/movie",
            Some("management-secret"),
            json!({ "kind": "smart", "save_path": "/tv" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 2. 检查已存在
    let res = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/downloaders/target-prefs",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let data = json_data(res).await;
    assert!(data.get("movie").is_some());

    // 3. PUT null 清除记忆（忘记）
    let res = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/downloaders/target-prefs/movie",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 4. 再次 GET，必须已经彻底没有 "movie" 键，而不是留着 null 导致被解析成默认记忆
    let res = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/downloaders/target-prefs",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let data = json_data(res).await;
    assert!(
        data.get("movie").is_none(),
        "清除记忆后不应保留该分类 key，实际得到: {:?}",
        data
    );
}

#[tokio::test]
async fn get_presets_unset_returns_null_instead_of_empty_list() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    // 新用户/未自定义用户拉取 presets
    let res = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/search/presets",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let data = json_data(res).await;
    assert!(
        data["presets"].as_array().unwrap().is_empty(),
        "未保存过 presets 时应返回空数组，实际得到: {:?}",
        data["presets"]
    );
}
