//! Library 浏览行级语义回归：NFO 决定的过滤/排序；嵌套根下各库只返回自己的文件。
//!
//! 原 `browse_snapshot.rs` 的门闩用例依赖已删除的浏览投影（投影在锁外读 NFO 时开闸），
//! 现在 `/items` 只读 `library_items` 摘要、不观测文件，门闩永不打开，故随快照层删除。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use api::ApiState;
use api::router;
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

#[path = "management/common.rs"]
mod common;
use common::{Fixtures, json_data, request, state};

fn base_state(tmp: &tempfile::TempDir) -> ApiState {
    let fixtures = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    state(tmp.path(), fixtures, Arc::new(MemoryDownloader::new(tmp.path().join("stage"))))
}

fn movie_library(state: &ApiState) -> (String, PathBuf) {
    let library = state
        .store()
        .lock()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.kind == MediaKind::Movie)
        .expect("default movie library");
    (library.id, library.root_paths[0].clone())
}

fn seed_row(state: &ApiState, media_id: MediaId, path: &Path, nfo: Option<&str>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, b"video-bytes").unwrap();
    if let Some(body) = nfo {
        std::fs::write(path.with_extension("nfo"), body).unwrap();
    }
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: path.to_string_lossy().to_string(),
        season: None,
        episode: None,
        resolution: Some("1080p".into()),
        codec: Some("H264".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
    };
    state.store().lock().insert_ledger(&row).unwrap();
}

fn insert_media(state: &ApiState, media_id: MediaId, title: &str) {
    let media = Media {
        id: media_id,
        kind: MediaKind::Movie,
        title: title.into(),
        year: Some(2024),
        original_title: Some(title.into()),
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    state.store().lock().insert_media(&media).unwrap();
}

async fn get_json(app: &axum::Router, url: &str) -> Value {
    let req = request("GET", url, Some("management-secret"), Value::Null);
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{url}");
    json_data(response).await
}

/// NFO 决定的过滤/排序仍按原语义生效（真实磁盘 NFO，发布时录入 `library_metadata_inputs`）。
#[tokio::test]
async fn nfo_metadata_still_drives_filter_and_sort() {
    let tmp = tempfile::tempdir().unwrap();
    let api_state = base_state(&tmp);
    let app = router(api_state.clone());
    let (library_id, root) = movie_library(&api_state);

    let high = MediaId::new();
    insert_media(&api_state, high, "High");
    seed_row(
        &api_state,
        high,
        &root.join("High").join("High.mkv"),
        Some("<movie><rating>8.5</rating><runtime>150</runtime><original_language>en</original_language></movie>"),
    );
    let low = MediaId::new();
    insert_media(&api_state, low, "Low");
    seed_row(
        &api_state,
        low,
        &root.join("Low").join("Low.mkv"),
        Some("<movie><rating>5.5</rating><runtime>90</runtime><original_language>ja</original_language></movie>"),
    );

    let all = get_json(
        &app,
        &format!("/api/v1/libraries/{library_id}/items?sort=rating&order=desc"),
    )
    .await;
    let titles: Vec<&str> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["High", "Low"], "rating desc 排序");

    let filtered = get_json(
        &app,
        &format!("/api/v1/libraries/{library_id}/items?rating_gte=8"),
    )
    .await;
    let titles: Vec<&str> = filtered
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["High"], "NFO rating 过滤");
    assert_eq!(filtered.as_array().unwrap()[0]["file_count"], 1);
}

/// 嵌套根 + 同一 Media 在两个库：各自只返回自己名下的文件。
#[tokio::test]
async fn nested_libraries_keep_their_own_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let api_state = base_state(&tmp);
    let app = router(api_state.clone());
    let (parent_id, parent_root) = movie_library(&api_state);

    let child_root = parent_root.join("child");
    std::fs::create_dir_all(&child_root).unwrap();
    let child = api_state
        .store()
        .lock()
        .create_library(
            MediaKind::Movie,
            "child",
            &[child_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let shared = MediaId::new();
    insert_media(&api_state, shared, "Shared");
    seed_row(
        &api_state,
        shared,
        &parent_root.join("Shared").join("Shared.mkv"),
        None,
    );
    seed_row(
        &api_state,
        shared,
        &child_root.join("Shared").join("Shared.mkv"),
        None,
    );

    let parent_items = get_json(&app, &format!("/api/v1/libraries/{parent_id}/items")).await;
    let parent_rows = parent_items.as_array().unwrap();
    assert_eq!(parent_rows.len(), 1);
    assert_eq!(
        parent_rows[0]["file_count"], 1,
        "父库不得把子库文件算进自己的条目"
    );

    let child_items = get_json(&app, &format!("/api/v1/libraries/{}/items", child.id)).await;
    let child_rows = child_items.as_array().unwrap();
    assert_eq!(child_rows.len(), 1);
    assert_eq!(child_rows[0]["file_count"], 1, "子库只应看到自己的文件");
}

