use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::SiteId;
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

async fn post_site(app: &axum::Router, body: Value) -> Value {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    json_data(response).await
}

#[tokio::test]
async fn site_cookie_payload_persists_cookie_without_api_key() {
    let tmp = tempfile::tempdir().unwrap();
    let created = post_site(
        &app(&tmp),
        json!({
            "name": "PTerClub",
            "url": "https://pterclub.net/",
            "profile_id": "pterclub",
            "cookie": "uid=1; pass=abc",
            "enabled": true
        }),
    )
    .await;
    let id = SiteId::from_str(created["id"].as_str().unwrap()).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let site = store.get_site(id).unwrap().expect("Site");
    assert_eq!(site.cookie.as_deref(), Some("uid=1; pass=abc"));
    assert!(site.api_key.is_none());
    assert_eq!(site.profile_id, "pterclub");
}

#[tokio::test]
async fn site_api_key_payload_persists_api_key_without_cookie() {
    let tmp = tempfile::tempdir().unwrap();
    let created = post_site(
        &app(&tmp),
        json!({
            "name": "M-Team",
            "url": "https://api.m-team.cc/",
            "profile_id": "mteam",
            "api_key": "secret-key",
            "enabled": true
        }),
    )
    .await;
    let id = SiteId::from_str(created["id"].as_str().unwrap()).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let site = store.get_site(id).unwrap().expect("Site");
    assert_eq!(site.api_key.as_deref(), Some("secret-key"));
    assert!(site.cookie.is_none());
    assert_eq!(site.profile_id, "mteam");
}

#[tokio::test]
async fn list_sites_omits_cookie_and_api_key() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    post_site(
        &app,
        json!({
            "name": "PTerClub",
            "url": "https://pterclub.net/",
            "profile_id": "pterclub",
            "cookie": "uid=1; pass=abc",
            "enabled": true
        }),
    )
    .await;
    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/sites",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_data(listed).await;
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["name"], "PTerClub");

    assert_eq!(row["profile_id"], "pterclub");
    assert_eq!(row["enabled"], true);
    assert_eq!(row["auth_type"], "cookie");
    assert!(row.get("cookie").is_none() || row["cookie"].is_null());
    assert!(row.get("api_key").is_none() || row["api_key"].is_null());
}

#[tokio::test]
async fn disabling_a_site_drops_it_from_search() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([(
            "search",
            include_str!("../../../indexer/tests/fixtures/nexusphp.html"),
        )]),
    });
    let app = router(state(
        tmp.path(),
        fetcher.clone(),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let created = post_site(
        &app,
        json!({
            "name": "demo",
            "url": "https://pt.example/",
            "profile_id": "demo",
            "cookie": "uid=1",
            "enabled": true
        }),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let patch = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/sites/{id}"),
            Some("management-secret"),
            json!({ "enabled": false }),
        ))
        .await
        .unwrap();
    assert_eq!(patch.status(), StatusCode::OK);
    let search = app
        .oneshot(request(
            "GET",
            "/api/v1/search/torrents?keyword=matrix",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(search.status(), StatusCode::OK);
    assert!(fetcher.requests.lock().is_empty());
}

#[tokio::test]
async fn delete_site_removes_row_and_404s_when_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let site = create_site(&app).await;
    let id = site["id"].as_str().unwrap();
    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/sites/{id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/sites",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(json_data(listed).await.as_array().unwrap().len(), 0);
    let again = app
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/sites/{id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn put_site_updates_fields_keeping_id() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let site = create_site(&app).await;
    let id = site["id"].as_str().unwrap();
    let updated = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/api/v1/sites/{id}"),
            Some("management-secret"),
            json!({
                "name": "PTerClub",
                "url": "https://pterclub.net/",
                "profile_id": "pterclub",
                "cookie": "uid=9; pass=zzz",
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    let body = json_data(updated).await;
    assert_eq!(body["id"], site["id"]);
    assert_eq!(body["name"], "PTerClub");
    assert_eq!(body["profile_id"], "pterclub");
    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/sites",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let list_body = json_data(listed).await;
    let list_rows = list_body.as_array().unwrap();
    assert_eq!(list_rows.len(), 1);
    assert_eq!(list_rows[0]["name"], "PTerClub");
}

#[tokio::test]
async fn create_site_rejects_empty_url_when_profile_has_no_base_url() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let res = app
        .oneshot(request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            json!({
                "name": "Custom",
                "url": "",
                "profile_id": "hdsky",
                "cookie": "uid=1; pass=abc",
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = json_body(res).await;
    assert_eq!(body["error"]["code"], "site.invalid");
}
