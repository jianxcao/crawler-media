//! Risk 3: organize may re-parent a file into another Library of the same kind,
//! but only when the request names that destination Library explicitly; it goes
//! through the durable rename path and the ledger follows.

use std::collections::HashMap;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::MediaKind;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

struct Fx {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    movie_id: String,
    other_id: String,
    tv_id: String,
    from: std::path::PathBuf,
    other_root: std::path::PathBuf,
    tv_root: std::path::PathBuf,
}

async fn get(app: &axum::Router, uri: &str) -> Value {
    json_body(
        app.clone()
            .oneshot(request("GET", uri, Some("management-secret"), Value::Null))
            .await
            .unwrap(),
    )
    .await
}

async fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader));
    let movie = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie).unwrap();
    let from = movie.join("Matrix.1999.mkv");
    std::fs::write(&from, b"matrix").unwrap();
    let libs = get(&app, "/api/v1/libraries").await;
    let movie_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    scan_and_wait(&app, &movie_id).await;
    let other_root = tmp.path().join("other-movies");
    let tv_root = tmp.path().join("other-tv");
    std::fs::create_dir_all(&other_root).unwrap();
    std::fs::create_dir_all(&tv_root).unwrap();
    let side = Store::open(tmp.path().join("data")).unwrap();
    let other = side
        .create_library(
            MediaKind::Movie,
            "Other",
            &[other_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let tv = side
        .create_library(
            MediaKind::Tv,
            "OtherTv",
            &[tv_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    Fx {
        _tmp: tmp,
        app,
        movie_id,
        other_id: other.id,
        tv_id: tv.id,
        from,
        other_root,
        tv_root,
    }
}

async fn organize(fx: &Fx, token: Option<&str>, entry: Value) -> (StatusCode, Value) {
    let response = fx
        .app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{}/organize", fx.movie_id),
            token,
            json!({ "renames": [entry] }),
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, json_body(response).await)
}

#[tokio::test]
async fn cross_library_organize_requires_named_destination() {
    let fx = fixture().await;
    let to = fx.other_root.join("Matrix (1999).mkv");
    let entry = json!({ "from": fx.from, "to": to });
    let (status, body) = organize(&fx, Some("management-secret"), entry).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["applied"], 0, "{body}");
    assert!(
        fx.from.is_file() && !to.exists(),
        "nothing moved without opt-in"
    );
}

#[tokio::test]
async fn cross_library_organize_reparents_through_durable_rename() {
    let fx = fixture().await;
    let to = fx.other_root.join("Matrix (1999).mkv");
    let entry = json!({ "from": fx.from, "to": to, "to_library_id": fx.other_id });
    let (status, body) = organize(&fx, Some("management-secret"), entry).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["applied"], 1, "{body}");
    assert!(to.is_file() && !fx.from.exists());
    let ledger = get(&fx.app, "/api/v1/ledger").await;
    let row = &ledger["data"][0];
    assert_eq!(row["path"], to.display().to_string());
    assert_eq!(row["library_id"], fx.other_id.as_str(), "{ledger}");
}

#[tokio::test]
async fn cross_library_organize_rejects_mismatched_or_other_kind_destination() {
    let fx = fixture().await;
    // Named Library does not own the target path.
    let to = fx.other_root.join("Matrix (1999).mkv");
    let entry = json!({ "from": fx.from, "to": to, "to_library_id": fx.tv_id });
    let (_, body) = organize(&fx, Some("management-secret"), entry).await;
    assert_eq!(body["data"]["applied"], 0, "{body}");
    // A TV Library root never accepts a movie file, even when named.
    let to_tv = fx.tv_root.join("Matrix (1999).mkv");
    let entry = json!({ "from": fx.from, "to": to_tv, "to_library_id": fx.tv_id });
    let (_, body) = organize(&fx, Some("management-secret"), entry).await;
    assert_eq!(body["data"]["applied"], 0, "{body}");
    assert!(fx.from.is_file() && !to.exists() && !to_tv.exists());
}

#[tokio::test]
async fn cross_library_organize_requires_admin() {
    let fx = fixture().await;
    let to = fx.other_root.join("Matrix (1999).mkv");
    let entry = json!({ "from": fx.from, "to": to, "to_library_id": fx.other_id });
    let (status, _) = organize(&fx, None, entry).await;
    assert!(
        status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
        "{status}"
    );
    assert!(fx.from.is_file() && !to.exists());
}
