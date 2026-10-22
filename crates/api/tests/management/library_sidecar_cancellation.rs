//! Frozen Library mutations must fail before filesystem work or catalog IO.
use super::common::*;
use api::{Store, router};
use axum::http::StatusCode;
use domain::{LedgerId, Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::json;
use std::{collections::HashMap, sync::Arc};
use tower::ServiceExt;
use store::test_support::{publish_fixture_file, FixtureFile};

#[tokio::test]
async fn frozen_library_rejects_organize_reidentify_and_sidecar_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let state = state(
        tmp.path(),
        fetcher,
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let root = tmp.path().join("extra-movies");
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("original.mkv");
    std::fs::write(&source, b"original-video").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Extra",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Original".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let file_id = LedgerId::new();
    let source_path = source.display().to_string();
    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(file_id),
            media_id: media.id,
            path: source_path.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    store.begin_library_removal(&library.id).unwrap();
    let app = router(state);
    let destination = root.join("new/renamed.mkv");
    let mutations = [
        (
            format!("/api/v1/libraries/{}/organize", library.id),
            json!({"renames":[{"from":source,"to":destination}]}),
        ),
        (
            "/api/v1/reidentify".into(),
            json!({"file_ids":[file_id.to_string()],"title_ref":"tmdb:movie:123"}),
        ),
        (
            "/api/v1/reidentify/extras".into(),
            json!({"file_ids":[file_id.to_string()]}),
        ),
        (
            format!(
                "/api/v1/libraries/{}/items/{}/metadata/refresh",
                library.id, media.id
            ),
            json!(null),
        ),
        (
            format!(
                "/api/v1/libraries/{}/items/{}/chapters/generate",
                library.id, media.id
            ),
            json!(null),
        ),
    ];
    for (url, body) in mutations {
        let response = app
            .clone()
            .oneshot(request("POST", &url, Some("management-secret"), body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT, "{url}");
    }
    assert_eq!(std::fs::read(&source).unwrap(), b"original-video");
    assert!(!destination.exists());
    assert!(!root.join("new").exists());
    assert!(!source.with_extension("nfo").exists());
    let coverage = store
        .file_coverage_by_id(file_id)
        .unwrap()
        .expect("published file must be readable by id");
    assert_eq!(coverage.file.path, std::path::PathBuf::from(&source_path));
    assert_eq!(
        store.get_media(media.id).unwrap().unwrap().title,
        "Original"
    );
}
