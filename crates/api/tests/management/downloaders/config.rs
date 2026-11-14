use super::*;

use api::{ChosenDownloader, DownloaderEnv, Store, choose_downloader};
use axum::http::StatusCode;
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn two_downloaders_one_default_and_list_omits_password() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let first = post_dl(
        &app,
        json!({
            "name": "qb-a",
            "kind": "qbittorrent",
            "url": "http://qb-a:8080",
            "username": "admin",
            "password": "secret-a",
            "is_default": true
        }),
    )
    .await;
    let second = post_dl(
        &app,
        json!({
            "name": "qb-b",
            "kind": "qbittorrent",
            "url": "http://qb-b:8080",
            "username": "admin",
            "password": "secret-b",
            "is_default": true
        }),
    )
    .await;
    assert_ne!(first["id"], second["id"]);

    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/downloaders",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_data(listed).await;
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let a = rows.iter().find(|row| row["name"] == "qb-a").unwrap();
    let b = rows.iter().find(|row| row["name"] == "qb-b").unwrap();
    assert_eq!(a["is_default"], false);
    assert_eq!(b["is_default"], true);
    assert!(a.get("password").is_none());
    assert!(b.get("password").is_none());

    let store = Store::open(tmp.path().join("data")).unwrap();
    let ChosenDownloader::Qbittorrent(cfg) =
        choose_downloader(&store, &DownloaderEnv::default()).unwrap()
    else {
        panic!("default row");
    };
    assert_eq!(cfg.url, "http://qb-b:8080");
    assert_eq!(cfg.password, "secret-b");
}

async fn assert_default_states(app: axum::Router) {
    let listed = json_data(
        app.oneshot(request(
            "GET",
            "/api/v1/downloaders",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    let a = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "qb-a")
        .unwrap();
    let b = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "qb-b")
        .unwrap();
    assert_eq!(a["is_default"], true);
    assert_eq!(b["is_default"], false);
}

#[tokio::test]
async fn patch_makes_existing_downloader_the_default() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let first = post_dl(
        &app,
        json!({
            "name": "qb-a",
            "kind": "qbittorrent",
            "url": "http://qb-a:8080",
            "username": "admin",
            "password": "a",
            "is_default": true
        }),
    )
    .await;
    post_dl(
        &app,
        json!({
            "name": "qb-b",
            "kind": "qbittorrent",
            "url": "http://qb-b:8080",
            "username": "admin",
            "password": "b",
            "is_default": false
        }),
    )
    .await;
    let id = first["id"].as_str().unwrap();
    let patched = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/downloaders/{id}"),
            Some("management-secret"),
            json!({ "is_default": true }),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    assert_default_states(app).await;
}
#[tokio::test]
async fn downloader_enabled_flag_round_trips_through_patch() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = post_dl(
        &app,
        json!({
            "name": "qb-a",
            "kind": "qbittorrent",
            "url": "http://qb-a:8080",
            "username": "admin",
            "password": "secret-a",
            "is_default": true
        }),
    )
    .await;
    assert_eq!(created["enabled"], true, "新建的下载器默认启用");
    let id = created["id"].as_str().unwrap().to_string();

    let response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/downloaders/{id}"),
            Some("management-secret"),
            json!({ "name": "qb-a", "enabled": false }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{response:?}");
    assert_eq!(json_data(response).await["enabled"], false);
}
