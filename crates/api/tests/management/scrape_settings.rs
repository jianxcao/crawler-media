//! Scrape & organize settings: defaults, save/validate, preview, and the
//! naming flow through the legacy /directory surface.

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

async fn get_json(app: &axum::Router, uri: &str) -> Value {
    json_body(
        app.clone()
            .oneshot(request("GET", uri, Some("management-secret"), Value::Null))
            .await
            .unwrap(),
    )
    .await
}

async fn put_json(app: &axum::Router, uri: &str, body: Value) -> Value {
    let response = app
        .clone()
        .oneshot(request("PUT", uri, Some("management-secret"), body))
        .await
        .unwrap();
    json_body(response).await
}

#[tokio::test]
async fn scrape_settings_defaults_equal_current_behavior() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let body = get_json(&app, "/api/v1/settings/scrape").await;
    let effective = &body["data"]["effective"];
    assert_eq!(effective["language_priority"][0], "zh-CN");
    assert_eq!(effective["language_priority"][1], "en-US");
    assert_eq!(effective["cert_country_priority"], json!(["CN", "US"]));
    assert_eq!(effective["poster_mode"], "default");
    assert_eq!(effective["poster_min_width"], 500);
    assert_eq!(effective["backdrop_min_width"], 1920);
    assert_eq!(effective["poster_size"], "w780");
    assert_eq!(effective["backdrop_size"], "original");
    assert_eq!(effective["still_size"], "w300");
    assert_eq!(effective["mirror_images"], true);
    assert_eq!(effective["mirror_nfo"], true);
    assert_eq!(effective["naming_entry_dir"], json!("{title} ({year})"));
}

#[tokio::test]
async fn scrape_settings_save_and_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let saved = put_json(
        &app,
        "/api/v1/settings/scrape",
        json!({
            "setting": {
                "language_priority": ["en-US", "zh-CN"],
                "cert_country_priority": ["US"],
                "poster_mode": "language",
                "poster_language_priority": ["meta", ""],
                "backdrop_language_priority": ["", "en"],
                "poster_min_width": 342,
                "mirror_images": false,
                "mirror_nfo": true
            }
        }),
    )
    .await;
    eprintln!("SAVED BODY: {}", serde_json::to_string(&saved).unwrap());
    assert_eq!(saved["data"]["effective"]["poster_mode"], "language");
    assert_eq!(saved["data"]["effective"]["poster_min_width"], 342);
    assert_eq!(saved["data"]["effective"]["mirror_images"], false);
    assert_eq!(saved["data"]["effective"]["language_priority"][0], "en-US");

    let body = get_json(&app, "/api/v1/settings/scrape").await;
    assert_eq!(body["data"]["effective"]["poster_mode"], "language");
    assert_eq!(body["data"]["setting"]["language_priority"][0], "en-US");
}

#[tokio::test]
async fn scrape_settings_validates_bad_naming() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let rejected = put_json(
        &app,
        "/api/v1/settings/scrape",
        json!({ "setting": { "naming_entry_dir": "Season X" } }),
    )
    .await;
    assert_eq!(rejected["error"]["code"], "scrape.invalid");
    let rejected = put_json(
        &app,
        "/api/v1/settings/scrape",
        json!({ "setting": { "naming_movie_file": "a/b/{title}{ext}" } }),
    )
    .await;
    assert_eq!(rejected["error"]["code"], "scrape.invalid");
    let rejected = put_json(
        &app,
        "/api/v1/settings/scrape",
        json!({ "setting": { "naming_movie_file": "{title}{nope}{ext}" } }),
    )
    .await;
    assert_eq!(rejected["error"]["code"], "scrape.invalid");
}

#[tokio::test]
async fn preview_naming_renders_sample_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/settings/scrape/preview-naming",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let body = json_body(response).await;
    let movie = body["data"]["movie"].as_str().unwrap();
    let tv = body["data"]["tv"].as_str().unwrap();
    assert!(movie.contains("黑客帝国"), "{movie}");
    assert!(movie.contains("1999"), "{movie}");
    assert!(tv.contains("S01E03"), "{tv}");
    assert!(tv.contains("Season"), "{tv}");
}

#[tokio::test]
async fn directory_naming_round_trips_through_scrape_config() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let saved = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/directory",
            Some("management-secret"),
            json!({
                "movie_root": tmp.path().join("m").display().to_string(),
                "tv_root": tmp.path().join("t").display().to_string(),
                "transfer_mode": "hardlink",
                "movie_naming": "{title}{ext}",
                "tv_naming": "{title} - {season_episode}{ext}",
                "scrape": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    let body = get_json(&app, "/api/v1/directory").await;
    let body = body["data"].clone();
    assert_eq!(body["movie_naming"], "{title}{ext}");
    assert_eq!(body["tv_naming"], "{title} - {season_episode}{ext}");

    let saved = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/directory",
            Some("management-secret"),
            json!({
                "movie_root": tmp.path().join("m").display().to_string(),
                "tv_root": tmp.path().join("t").display().to_string(),
                "transfer_mode": "hardlink",
                "movie_naming": "{title} ({year})/{title}{ext}",
                "tv_naming": "{title} ({year})/Season {season}/{title} - {season_episode}{ext}",
                "scrape": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    let body = get_json(&app, "/api/v1/directory").await;
    let body = body["data"].clone();
    assert_eq!(body["movie_naming"], "{title} ({year})/{title}{ext}");
    assert_eq!(
        body["tv_naming"],
        "{title} ({year})/Season {season}/{title} - {season_episode}{ext}"
    );
}

#[tokio::test]
async fn member_reading_scrape_settings_redacts_theintrodb_api_key() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    // 1. 管理员配置 theintrodb_api_key
    let _ = put_json(
        &app,
        "/api/v1/settings/scrape",
        json!({
            "setting": {
                "theintrodb_enabled": true,
                "theintrodb_api_key": "secret-intro-key-12345"
            }
        }),
    )
    .await;

    // 管理员读取能看到真实 key
    let admin_read = get_json(&app, "/api/v1/settings/scrape").await;
    assert_eq!(
        admin_read["data"]["setting"]["theintrodb_api_key"],
        "secret-intro-key-12345"
    );

    // 2. 创建一个普通成员用户
    let create_user_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({
                "login": "normal-user",
                "password": "user-pass-123",
                "role": "member"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(create_user_res.status(), StatusCode::CREATED);

    // 普通成员登录
    let login_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({
                "username": "normal-user",
                "password": "user-pass-123"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(login_res.status(), StatusCode::OK);
    let login_body = json_body(login_res).await;
    let member_token = login_body["data"]["token"].as_str().unwrap();

    // 3. 普通成员读取 /settings/scrape
    let member_read = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/settings/scrape",
                Some(member_token),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;

    // 必须脱敏为 null
    assert_eq!(
        member_read["data"]["setting"]["theintrodb_api_key"],
        Value::Null,
        "普通成员读取 scrape settings 时必须脱敏 API key"
    );
}
