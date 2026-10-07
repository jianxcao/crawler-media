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
async fn directory_get_returns_seeded_library_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let listed = app(&tmp)
        .oneshot(request(
            "GET",
            "/api/v1/directory",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let body = json_data(listed).await;
    assert!(body["movie_root"].as_str().unwrap().ends_with("movies"));
    assert!(body["tv_root"].as_str().unwrap().ends_with("tv"));
    assert_eq!(body["transfer_mode"], "hardlink");
    assert!(body["movie_naming"].as_str().unwrap().contains("{title}"));
    assert!(body["tv_naming"].as_str().unwrap().contains("Season"));
    assert_eq!(body["scrape"], false);
}

#[tokio::test]
async fn directory_put_round_trips_roots_and_transfer_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let movie = tmp.path().join("lib/movies");
    let tv = tmp.path().join("lib/tv");
    let saved = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/directory",
            Some("management-secret"),
            json!({
                "movie_root": movie.display().to_string(),
                "tv_root": tv.display().to_string(),
                "transfer_mode": "copy",
                "movie_naming": "{title}{ext}",
                "tv_naming": "{title} - {season_episode}{ext}",
                "scrape": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    let body = json_data(
        app.oneshot(request(
            "GET",
            "/api/v1/directory",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(body["movie_root"], movie.display().to_string());
    assert_eq!(body["tv_root"], tv.display().to_string());
    assert_eq!(body["transfer_mode"], "copy");
    assert_eq!(body["movie_naming"], "{title}{ext}");
    assert_eq!(body["tv_naming"], "{title} - {season_episode}{ext}");
    assert_eq!(body["scrape"], true);
}

#[tokio::test]
async fn directory_posts_extra_library_root() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let movie = tmp.path().join("lib/movies");
    let extra = tmp.path().join("disk2/movies");
    let tv = tmp.path().join("lib/tv");
    let saved = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/directory",
            Some("management-secret"),
            json!({
                "movie_root": movie.display().to_string(),
                "tv_root": tv.display().to_string(),
                "transfer_mode": "hardlink",
                "movie_naming": "{title}{ext}",
                "tv_naming": "{title} - {season_episode}{ext}",
                "scrape": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);

    let added = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/directory/roots",
            Some("management-secret"),
            json!({
                "kind": "movie",
                "path": extra.display().to_string()
            }),
        ))
        .await
        .unwrap();
    assert_eq!(added.status(), StatusCode::CREATED);
    let row = json_data(added).await;
    assert_eq!(row["kind"], "movie");
    assert_eq!(row["path"], extra.display().to_string());
    assert_eq!(row["is_default"], false);

    let listed = json_data(
        app.oneshot(request(
            "GET",
            "/api/v1/directory",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(listed["movie_root"], movie.display().to_string());
    let extras = listed["extra_roots"].as_array().unwrap();
    assert_eq!(extras.len(), 1);
    assert_eq!(extras[0]["kind"], "movie");
    assert_eq!(extras[0]["path"], extra.display().to_string());
    assert_eq!(extras[0]["is_default"], false);
}

#[tokio::test]
async fn directory_deletes_extra_library_root() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let movie = tmp.path().join("lib/movies");
    let extra = tmp.path().join("disk2/movies");
    let tv = tmp.path().join("lib/tv");
    app.clone()
        .oneshot(request(
            "PUT",
            "/api/v1/directory",
            Some("management-secret"),
            json!({
                "movie_root": movie.display().to_string(),
                "tv_root": tv.display().to_string(),
                "transfer_mode": "hardlink",
                "movie_naming": "{title}{ext}",
                "tv_naming": "{title} - {season_episode}{ext}",
                "scrape": false
            }),
        ))
        .await
        .unwrap();
    let added = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/directory/roots",
                Some("management-secret"),
                json!({
                    "kind": "movie",
                    "path": extra.display().to_string()
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = added["id"].as_str().unwrap();
    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/directory/roots/{id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    let listed = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/directory",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(listed["extra_roots"].as_array().unwrap().len(), 0);
    assert_eq!(listed["movie_root"], movie.display().to_string());
}

#[tokio::test]
async fn directory_rejects_deleting_default_library_root() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let listed = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/directory",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = listed["movie_root_id"].as_str().unwrap();
    let rejected = app
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/directory/roots/{id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
}
