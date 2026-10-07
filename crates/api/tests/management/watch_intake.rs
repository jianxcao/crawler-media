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

async fn put_intake(app: &axum::Router, tmp: &tempfile::TempDir, intake: &std::path::Path) {
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
                "transfer_mode": "copy",
                "movie_naming": "{title}{ext}",
                "tv_naming": "{title} - {season_episode}{ext}",
                "scrape": false,
                "watch_intake": intake.display().to_string()
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
async fn watch_intake_transfers_high_confidence_drop_into_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    std::fs::create_dir_all(&intake).unwrap();
    std::fs::write(intake.join("The.Matrix.1999.2160p.BluRay.mkv"), b"video").unwrap();
    let app = app(&tmp);
    put_intake(&app, &tmp, &intake).await;
    tick(&app, 1).await;

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
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["media_title"], "The Matrix");
    assert!(std::path::Path::new(rows[0]["path"].as_str().unwrap()).is_file());
}

#[tokio::test]
async fn watch_intake_parks_low_confidence_as_unidentified() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    std::fs::create_dir_all(&intake).unwrap();
    let drop = intake.join("foo.mkv");
    std::fs::write(&drop, b"??").unwrap();
    let app = app(&tmp);
    put_intake(&app, &tmp, &intake).await;
    tick(&app, 1).await;

    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/unidentified",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let rows = json_data(listed).await;
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["path"], drop.display().to_string());
    assert_eq!(row["confidence"], "low");
}

#[tokio::test]
async fn watch_intake_partial_success_records_non_conflicting_files() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    let movie = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&intake).unwrap();
    std::fs::create_dir_all(&movie).unwrap();

    // 预先存在冲突文件
    let conflict_dest = movie.join("Inception.2010.1080p.BluRay.mkv");
    std::fs::write(&conflict_dest, b"existing-inception").unwrap();

    // Intake 放入两个文件：正常 Matrix 和冲突 Inception
    let good_file = intake.join("The.Matrix.1999.2160p.BluRay.mkv");
    let conflict_file = intake.join("Inception.2010.1080p.BluRay.mkv");
    std::fs::write(&good_file, b"matrix-data").unwrap();
    std::fs::write(&conflict_file, b"new-inception-data").unwrap();

    let app = app(&tmp);
    put_intake(&app, &tmp, &intake).await;
    tick(&app, 1).await;

    // 冲突文件不能覆盖已有文件
    assert_eq!(std::fs::read(&conflict_dest).unwrap(), b"existing-inception");

    // 正常文件成功进入 ledger
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
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["media_title"], "The Matrix");
}
