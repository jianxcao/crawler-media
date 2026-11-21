//! Organize: preview computes template targets, apply renames files and
//! updates the ledger; gallery groups images; single-item refresh 200s.

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

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

async fn seed_movie_lib(app: &axum::Router, tmp: &tempfile::TempDir) -> (String, String) {
    let movie = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie).unwrap();
    // 文件名不合命名模板：标题带点号、无年份括号。
    std::fs::write(movie.join("Matrix.1999.mkv"), b"matrix").unwrap();
    std::fs::write(movie.join("poster.jpg"), b"poster").unwrap();
    let libs = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/libraries",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let movie_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    scan_and_wait(app, &movie_id).await;
    let ledger = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ledger["data"][0]["library_id"], movie_id);
    let media_id = ledger["data"][0]["media_id"].as_str().unwrap().to_string();
    (movie_id, media_id)
}

#[tokio::test]
async fn organize_preview_and_apply_renames_files() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (movie_id, _media_id) = seed_movie_lib(&app, &tmp).await;
    let movie = tmp.path().join("data/library/movies");
    let original = movie.join("Matrix.1999.mkv");
    assert!(original.is_file());

    let preview = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/organize-preview"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        !preview["data"]["renames"].as_array().unwrap().is_empty(),
        "{preview}"
    );
    let from = preview["data"]["renames"][0]["from"].as_str().unwrap();
    let to = preview["data"]["renames"][0]["to"].as_str().unwrap();
    assert_eq!(from, original.display().to_string());
    assert!(to.contains("Matrix (1999)"), "目标名按命名模板生成: {to}");

    let applied = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{movie_id}/organize"),
            Some("management-secret"),
            json!({ "renames": [{ "from": from, "to": to }] }),
        ))
        .await
        .unwrap();
    assert_eq!(applied.status(), StatusCode::OK);
    let body = json_body(applied).await;
    assert_eq!(body["data"]["applied"], 1, "{body}");
    assert!(std::path::Path::new(to).is_file(), "file renamed: {to}");
    assert!(!original.exists(), "old name gone");

    let ledger = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        ledger["data"][0]["path"], to,
        "ledger path follows the rename"
    );
}

#[tokio::test]
async fn gallery_and_item_refresh_respond() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (movie_id, media_id) = seed_movie_lib(&app, &tmp).await;

    let gallery = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/gallery"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let groups = gallery["data"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "gallery has the owned item group");
    assert_eq!(groups[0]["title"], "Matrix");

    let refresh = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{movie_id}/items/{media_id}/metadata/refresh"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(refresh.status(), StatusCode::OK);
}

#[tokio::test]
async fn item_refresh_rejects_media_owned_by_another_library() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (_movie_id, media_id) = seed_movie_lib(&app, &tmp).await;
    let other_root = tmp.path().join("other-movies");
    std::fs::create_dir_all(&other_root).unwrap();
    let other = Store::open(tmp.path().join("data"))
        .unwrap()
        .create_library(
            MediaKind::Movie,
            "Other",
            &[other_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let refresh = app
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/libraries/{}/items/{media_id}/metadata/refresh",
                other.id
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(refresh.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn p2_8_cross_library_organize_preview_includes_already_ok_files_for_target_library_and_root()
{
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let store = Store::open(tmp.path().join("data")).unwrap();
    let src_root = tmp.path().join("movies-src");
    let dst_root_1 = tmp.path().join("movies-dst-1");
    let dst_root_2 = tmp.path().join("movies-dst-2");
    let tv_root = tmp.path().join("tv-other");
    for dir in [&src_root, &dst_root_1, &dst_root_2, &tv_root] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let src_lib = store
        .create_library(
            MediaKind::Movie,
            "Movies A",
            &[src_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let dst_lib = store
        .create_library(
            MediaKind::Movie,
            "Movies B",
            &[dst_root_1.to_str().unwrap(), dst_root_2.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let tv_lib = store
        .create_library(
            MediaKind::Tv,
            "TV Other",
            &[tv_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    // Seed one movie already conforming to template under src_lib
    use api::ScrapeStoreExt;
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: MediaKind::Movie,
        title: "Inception".into(),
        year: Some(2010),
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let pattern = store.naming_pattern(MediaKind::Movie).unwrap();
    let conforming = library::render_path(
        &src_root,
        &pattern,
        &media,
        &release::parse("Inception.2010.mkv"),
        std::path::Path::new("Inception.2010.mkv"),
    )
    .unwrap();
    std::fs::create_dir_all(conforming.parent().unwrap()).unwrap();
    std::fs::write(&conforming, b"movie-bytes").unwrap();
    store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: media.id,
            path: conforming.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();

    // Same-library preview sees already_ok=1, renames=[]
    let same_res = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{}/organize-preview", src_lib.id),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(same_res.status(), StatusCode::OK);
    let same_body = json_body(same_res).await;
    assert_eq!(same_body["data"]["already_ok"], 1);
    assert_eq!(same_body["data"]["renames"].as_array().unwrap().len(), 0);

    // Cross-library preview against dst_lib + dst_root_2 includes the conforming file!
    let encoded_root2 =
        url::form_urlencoded::byte_serialize(dst_root_2.to_str().unwrap().as_bytes())
            .collect::<String>();
    let cross_res = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/libraries/{}/organize-preview?target_library_id={}&target_root={encoded_root2}",
                src_lib.id, dst_lib.id
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(cross_res.status(), StatusCode::OK);
    let cross_body = json_body(cross_res).await;
    assert_eq!(cross_body["data"]["already_ok"], 0);
    let renames = cross_body["data"]["renames"].as_array().unwrap();
    assert_eq!(renames.len(), 1);
    let dest_path = renames[0]["to"].as_str().unwrap().to_string();
    assert!(
        dest_path.starts_with(dst_root_2.to_str().unwrap()),
        "expected target under dst_root_2, got {dest_path}"
    );

    // Mismatched kind and unknown target_root are rejected with 400
    let bad_kind = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/libraries/{}/organize-preview?target_library_id={}",
                src_lib.id, tv_lib.id
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(bad_kind.status(), StatusCode::BAD_REQUEST);

    // Applying the previewed cross-library rename moves the file to dst_root_2 & re-parents to dst_lib
    let apply_res = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{}/organize", src_lib.id),
            Some("management-secret"),
            json!({
                "renames": [{
                    "from": renames[0]["from"],
                    "to": dest_path,
                    "to_library_id": dst_lib.id,
                }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(apply_res.status(), StatusCode::OK);
    assert!(std::path::Path::new(&dest_path).is_file());
    assert!(!conforming.exists());
    let moved = store.file_coverage_by_path(&dest_path).unwrap().unwrap();
    assert_eq!(moved.file.library_id.to_string(), dst_lib.id);
}
