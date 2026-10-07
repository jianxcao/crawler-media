//! Per-library visibility: members see everyone + selected; admins see all.

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

async fn admin_json(app: &axum::Router, uri: &str) -> Value {
    json_body(
        app.clone()
            .oneshot(request("GET", uri, Some("management-secret"), Value::Null))
            .await
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn member_only_sees_visible_libraries() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    // 建一个「仅选定成员」库，并把第二个成员设为可见。
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": "sister", "password": "pw" }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let member_id = json_body(created).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let lib = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/libraries",
            Some("management-secret"),
            json!({
                "name": "儿童区",
                "kind": "movie",
                "root_paths": [tmp.path().join("kids").display().to_string()],
                "access_mode": "selected",
                "admin_visible": true,
                "member_ids": [member_id],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(lib.status(), StatusCode::OK);
    let library_id = json_body(lib).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // A one-field PATCH must retain both the selected-member restriction and
    // omitted intro settings.
    let patched = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/libraries/{library_id}"),
            Some("management-secret"),
            json!({
                "member_ids": [member_id],
                "enable_fingerprint": true,
                "realtime_watch": false,
                "exclude_from_home": true,
                "generate_thumbnails": false,
                "extract_chapter_images": false,
                "auto_series_collections": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    let patched = json_body(patched).await;
    assert_eq!(patched["data"]["access_mode"], "selected");
    assert_eq!(patched["data"]["detect_intros"], true);
    assert_eq!(patched["data"]["enable_fingerprint"], true);
    assert_eq!(patched["data"]["realtime_watch"], false);
    assert_eq!(patched["data"]["exclude_from_home"], true);
    assert_eq!(patched["data"]["generate_thumbnails"], false);
    assert_eq!(patched["data"]["extract_chapter_images"], false);
    assert_eq!(patched["data"]["auto_series_collections"], false);

    // 管理 token 建一个成员 token 直接访问：先用 admin 登录拿成员凭据。
    let login = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            Some("management-secret"),
            json!({ "username": "sister", "password": "pw" }),
        ))
        .await
        .unwrap();
    eprintln!("LOGIN status={}", login.status());
    let login_body = json_body(login).await;
    eprintln!("LOGIN body={}", login_body);
    let member_token = login_body["data"]["token"].as_str().unwrap().to_string();
    let member_resp = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/libraries",
            Some(&member_token),
            Value::Null,
        ))
        .await
        .unwrap();
    eprintln!("MEMBER LIBS status={}", member_resp.status());
    let member_bytes = axum::body::to_bytes(member_resp.into_body(), 8192)
        .await
        .unwrap();
    eprintln!(
        "MEMBER LIBS body={}",
        String::from_utf8_lossy(&member_bytes)
    );
    let admin_libs_debug = admin_json(&app, "/api/v1/libraries").await;
    eprintln!("ADMIN LIBS={}", admin_libs_debug);
    let member_libs = json_body(axum::response::Response::new(axum::body::Body::from(
        member_bytes,
    )))
    .await;
    let names: Vec<&str> = member_libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"儿童区"),
        "selected member sees the library: {names:?}"
    );
    assert!(names.contains(&"电影库"));

    // 第三个成员（不在 member_ids）看不到「儿童区」。
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": "cousin", "password": "pw" }),
        ))
        .await
        .unwrap();
    let _ = created;
    let login = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            Some("management-secret"),
            json!({ "username": "cousin", "password": "pw" }),
        ))
        .await
        .unwrap();
    let cousin_token = json_body(login).await["data"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    let cousin_libs = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/libraries",
                Some(&cousin_token),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let names: Vec<&str> = cousin_libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"儿童区"),
        "unlisted member hidden: {names:?}"
    );

    // 管理员仍看到全部。
    let admin_libs = admin_json(&app, "/api/v1/libraries").await;
    let names: Vec<&str> = admin_libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"儿童区"));
}
