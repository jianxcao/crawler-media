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

#[tokio::test]
async fn put_site_falls_back_to_profile_base_url_or_rejects_empty_url_and_sets_auth_type() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let hdarea = post_site(
        &app,
        json!({
            "name": "HDArea",
            "url": "https://www.hdarea.co",
            "profile_id": "hdarea",
            "api_key": "0123456789abcdef0123456789abcdef",
            "enabled": true
        }),
    )
    .await;
    let hdarea_id = hdarea["id"].as_str().unwrap();
    let put_hdarea = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/api/v1/sites/{hdarea_id}"),
            Some("management-secret"),
            json!({
                "name": "HDArea",
                "url": "   ",
                "profile_id": "hdarea",
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(put_hdarea.status(), StatusCode::OK);
    let hdarea_body = json_data(put_hdarea).await;
    assert_eq!(hdarea_body["url"], "https://hdarea.club");
    assert_eq!(hdarea_body["auth_type"], "apikey");

    let hdsky = post_site(
        &app,
        json!({
            "name": "HDSky",
            "url": "https://hdsky.me/",
            "profile_id": "hdsky",
            "cookie": "uid=1; pass=abc",
            "enabled": true
        }),
    )
    .await;
    let hdsky_id = hdsky["id"].as_str().unwrap();
    let put_empty = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/api/v1/sites/{hdsky_id}"),
            Some("management-secret"),
            json!({
                "name": "HDSky",
                "url": "",
                "profile_id": "hdsky",
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(put_empty.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(put_empty).await["error"]["code"], "site.invalid");

    let store = Store::open(tmp.path().join("data")).unwrap();
    let stored = store
        .get_site(SiteId::from_str(hdsky_id).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(stored.url, "https://hdsky.me/");
}

#[tokio::test]
async fn site_write_responses_mask_cookie_and_api_key_consistently() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = post_site(
        &app,
        json!({
            "name": "MaskedWrite",
            "url": "https://pterclub.net/",
            "profile_id": "pterclub",
            "cookie": "uid=1; pass=topsecret",
            "enabled": true
        }),
    )
    .await;
    assert_eq!(created["auth_type"], "cookie");
    assert!(created["cookie"].is_null());
    assert!(created["api_key"].is_null());

    let id = created["id"].as_str().unwrap();
    let updated = json_data(
        app.oneshot(request(
            "PUT",
            &format!("/api/v1/sites/{id}"),
            Some("management-secret"),
            json!({
                "name": "MaskedWrite Updated",
                "url": "https://pterclub.net/",
                "profile_id": "pterclub",
                "enabled": true
            }),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(updated["auth_type"], "cookie");
    assert!(updated["cookie"].is_null());
    assert!(updated["api_key"].is_null());
}

#[tokio::test]
async fn patch_site_empty_url_falls_back_to_profile_base_url_or_rejects_missing_base_url() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let hdarea = post_site(
        &app,
        json!({
            "name": "HDArea",
            "url": "https://www.hdarea.co",
            "profile_id": "hdarea",
            "api_key": "0123456789abcdef0123456789abcdef",
            "enabled": true
        }),
    )
    .await;
    let hdarea_id = hdarea["id"].as_str().unwrap();

    let omit_url = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/sites/{hdarea_id}"),
            Some("management-secret"),
            json!({ "name": "HDArea Custom" }),
        ))
        .await
        .unwrap();
    assert_eq!(omit_url.status(), StatusCode::OK);
    assert_eq!(json_data(omit_url).await["url"], "https://www.hdarea.co");

    let reset_url = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/sites/{hdarea_id}"),
            Some("management-secret"),
            json!({ "url": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(reset_url.status(), StatusCode::OK);
    assert_eq!(json_data(reset_url).await["url"], "https://hdarea.club");

    let hdsky = post_site(
        &app,
        json!({
            "name": "HDSky",
            "url": "https://hdsky.me/",
            "profile_id": "hdsky",
            "cookie": "uid=1; pass=abc",
            "enabled": true
        }),
    )
    .await;
    let hdsky_id = hdsky["id"].as_str().unwrap();
    let patch_empty = app
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/sites/{hdsky_id}"),
            Some("management-secret"),
            json!({ "url": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(patch_empty.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(patch_empty).await["error"]["code"],
        "site.invalid"
    );

    let store = Store::open(tmp.path().join("data")).unwrap();
    let stored = store
        .get_site(SiteId::from_str(hdsky_id).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(stored.url, "https://hdsky.me/");
}

#[tokio::test]
async fn put_switching_api_site_to_cookie_only_nexusphp_clears_api_key_and_auth_type() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let created = post_site(
        &app,
        json!({
            "name": "M-Team",
            "url": "https://api.m-team.cc/",
            "profile_id": "mteam",
            "api_key": "secret-mteam-key",
            "enabled": true
        }),
    )
    .await;
    assert_eq!(created["auth_type"], "apikey");
    let id = created["id"].as_str().unwrap();

    let switched = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/api/v1/sites/{id}"),
            Some("management-secret"),
            json!({
                "name": "PTerClub",
                "url": "https://pterclub.net/",
                "profile_id": "pterclub",
                "cookie": "uid=42; pass=xyz",
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(switched.status(), StatusCode::OK);
    let switched_body = json_data(switched).await;
    assert_eq!(switched_body["auth_type"], "cookie");

    let store = Store::open(tmp.path().join("data")).unwrap();
    let stored = store
        .get_site(SiteId::from_str(id).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(stored.profile_id, "pterclub");
    assert_eq!(stored.cookie.as_deref(), Some("uid=42; pass=xyz"));
    assert_eq!(
        stored.api_key, None,
        "cookie-only NexusPHP profile must not retain prior API key"
    );

    let detail = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/sites/{id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(detail["auth_type"], "cookie");

    let switched_no_cookie = json_data(
        app.oneshot(request(
            "PUT",
            &format!("/api/v1/sites/{id}"),
            Some("management-secret"),
            json!({
                "name": "HDSky",
                "url": "https://hdsky.me/",
                "profile_id": "hdsky",
                "cookie": "",
                "api_key": "ignored-on-cookie-only",
                "enabled": true
            }),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(switched_no_cookie["auth_type"], "none");
    let stored_after = store
        .get_site(SiteId::from_str(id).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(stored_after.api_key, None);
}

#[tokio::test]
async fn create_and_patch_site_reject_invalid_downloader_id_and_clear_on_null_or_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let dl_id = domain::DownloaderId::new();

    let bad_post = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            json!({
                "name": "PTerClub",
                "url": "https://pterclub.net/",
                "profile_id": "pterclub",
                "cookie": "uid=1",
                "downloader_id": "not-a-uuid",
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(bad_post.status(), StatusCode::BAD_REQUEST);
    let bad_post_err = json_body(bad_post).await;
    assert_eq!(bad_post_err["error"]["code"], "site.invalid");
    assert_eq!(bad_post_err["error"]["message"], "下载器 id 无效");

    let created = post_site(
        &app,
        json!({
            "name": "PTerClub",
            "url": "https://pterclub.net/",
            "profile_id": "pterclub",
            "cookie": "uid=1",
            "downloader_id": dl_id.to_string(),
            "enabled": true
        }),
    )
    .await;
    assert_eq!(created["downloader_id"], dl_id.to_string());
    let id = created["id"].as_str().unwrap();

    let bad_patch = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/sites/{id}"),
            Some("management-secret"),
            json!({ "downloader_id": "not-a-uuid" }),
        ))
        .await
        .unwrap();
    assert_eq!(bad_patch.status(), StatusCode::BAD_REQUEST);
    let bad_patch_err = json_body(bad_patch).await;
    assert_eq!(bad_patch_err["error"]["code"], "site.invalid");
    assert_eq!(bad_patch_err["error"]["message"], "下载器 id 无效");

    let store = Store::open(tmp.path().join("data")).unwrap();
    let unchanged = store
        .get_site(SiteId::from_str(id).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        unchanged.downloader_id,
        Some(dl_id),
        "invalid PATCH downloader_id must not wipe existing binding"
    );

    let clear_null = json_data(
        app.clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/v1/sites/{id}"),
                Some("management-secret"),
                json!({ "downloader_id": Value::Null }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(clear_null["downloader_id"].is_null());

    let rebind = json_data(
        app.clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/v1/sites/{id}"),
                Some("management-secret"),
                json!({ "downloader_id": dl_id.to_string() }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(rebind["downloader_id"], dl_id.to_string());

    let clear_empty = json_data(
        app.oneshot(request(
            "PATCH",
            &format!("/api/v1/sites/{id}"),
            Some("management-secret"),
            json!({ "downloader_id": "   " }),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert!(clear_empty["downloader_id"].is_null());
}
