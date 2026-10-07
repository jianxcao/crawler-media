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

async fn put_inplace(app: &axum::Router, tmp: &tempfile::TempDir, watch: &std::path::Path) {
    let movie = tmp.path().join("data/library/movies");
    let tv = tmp.path().join("data/library/tv");
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
                "movie_naming": "{title} ({year})/{title} ({year}){ext}",
                "tv_naming": "{title} - {season_episode}{ext}",
                "scrape": true,
                "watch_inplace": watch.display().to_string()
            }),
        ))
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
}

async fn tick(app: &axum::Router, now: i64) {
    for _ in 0..8 {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn directory_round_trips_watch_inplace() {
    let tmp = tempfile::tempdir().unwrap();
    let watch = tmp.path().join("existing");
    let app = app(&tmp);
    put_inplace(&app, &tmp, &watch).await;
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
    assert_eq!(body["watch_inplace"], watch.display().to_string());
}

#[tokio::test]
async fn scrape_job_does_not_write_placeholder_nfo_before_identification() {
    let tmp = tempfile::tempdir().unwrap();
    let watch = tmp.path().join("existing");
    std::fs::create_dir_all(&watch).unwrap();
    let video = watch.join("The.Matrix.1999.2160p.BluRay.mkv");
    std::fs::write(&video, b"stay").unwrap();
    let app = app(&tmp);
    put_inplace(&app, &tmp, &watch).await;
    tick(&app, 1).await;

    assert_eq!(std::fs::read(&video).unwrap(), b"stay");
    let nfo = watch.join("The.Matrix.1999.2160p.BluRay.nfo");
    assert!(
        !nfo.exists(),
        "unidentified file should not get a placeholder NFO"
    );

    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let rows = json_data(listed).await;
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["media_title"], "The Matrix");
    assert_eq!(row["path"], video.display().to_string());

    let wall = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/libraries",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let libs = json_body(wall).await;
    let movie_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let wall = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{movie_id}/items"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let cards = json_body(wall).await;
    assert_eq!(cards["data"][0]["title"], "The Matrix");
}

#[tokio::test]
async fn scrape_job_scans_extra_library_root_into_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let extra = tmp.path().join("disk2/movies");
    std::fs::create_dir_all(&extra).unwrap();
    let video = extra.join("The.Matrix.1999.2160p.BluRay.mkv");
    std::fs::write(&video, b"stay").unwrap();
    let app = app(&tmp);
    let movie = tmp.path().join("data/library/movies");
    let tv = tmp.path().join("data/library/tv");
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
                "movie_naming": "{title} ({year})/{title} ({year}){ext}",
                "tv_naming": "{title} - {season_episode}{ext}",
                "scrape": true
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
    tick(&app, 1).await;

    assert_eq!(std::fs::read(&video).unwrap(), b"stay");
    assert!(!movie.join("The.Matrix.1999.2160p.BluRay.mkv").exists());

    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let rows = json_data(listed).await;
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["media_title"], "The Matrix");
    assert_eq!(row["path"], video.display().to_string());

    let wall = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/libraries",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let libs = json_body(wall).await;
    let movie_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let wall = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{movie_id}/items"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let cards = json_body(wall).await;
    assert_eq!(cards["data"][0]["title"], "The Matrix");
}

#[tokio::test]
async fn scrape_job_scans_extra_tv_root_for_strm_into_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let extra = tmp.path().join("disk2/tv");
    let nested = extra.join("The Long Watch/Season 01");
    std::fs::create_dir_all(&nested).unwrap();
    let video = nested.join("The Long Watch - S01E01 - 第 1 集.strm");
    std::fs::write(&video, b"stay").unwrap();
    let app = app(&tmp);
    let movie = tmp.path().join("data/library/movies");
    let tv = tmp.path().join("data/library/tv");
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
                "movie_naming": "{title} ({year})/{title} ({year}){ext}",
                "tv_naming": "{title} - {season_episode}{ext}",
                "scrape": true
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
                "kind": "tv",
                "path": extra.display().to_string()
            }),
        ))
        .await
        .unwrap();
    assert_eq!(added.status(), StatusCode::CREATED);
    tick(&app, 1).await;

    assert_eq!(std::fs::read(&video).unwrap(), b"stay");
    assert!(!tv.join("The.Long.Watch.S01E01.1080p.mkv").exists());
    assert!(
        !nested
            .join("The Long Watch - S01E01 - 第 1 集.nfo")
            .exists()
    );

    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let rows = json_data(listed).await;
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["media_title"], "The Long Watch");
    assert_eq!(row["path"], video.display().to_string());
}
