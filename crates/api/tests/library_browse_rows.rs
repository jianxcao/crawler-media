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
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

#[path = "management/common.rs"]
mod common;
use common::{Fixtures, json_data, request, state};

fn base_state(tmp: &tempfile::TempDir) -> ApiState {
    let fixtures = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    state(
        tmp.path(),
        fixtures,
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
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
    let spec = FixtureFile {
        file_id: Some(LedgerId::new()),
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
        ..Default::default()
    };
    publish_fixture_file(&state.store().lock(), &spec).unwrap();
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
        Some(
            "<movie><rating>8.5</rating><runtime>150</runtime><original_language>en</original_language></movie>",
        ),
    );
    let low = MediaId::new();
    insert_media(&api_state, low, "Low");
    seed_row(
        &api_state,
        low,
        &root.join("Low").join("Low.mkv"),
        Some(
            "<movie><rating>5.5</rating><runtime>90</runtime><original_language>ja</original_language></movie>",
        ),
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

// ---------------------------------------------------------------------------
// 文件分页契约：每个已发布文件恰好出现一次，末页 `next_cursor` 为 null，
// pending 工作态不进入浏览页，多集文件携带完整 units。
// ---------------------------------------------------------------------------

fn published_file_row(
    library_id: &str,
    root_id: &str,
    media_id: MediaId,
    index: usize,
) -> domain::LibraryFile {
    domain::LibraryFile {
        file_id: LedgerId::new(),
        library_id: library_id.parse().unwrap(),
        root_id: root_id.to_string(),
        media_id,
        kind: MediaKind::Movie,
        path: PathBuf::from(format!("Paged/version-{index:03}.mkv")),
        version: domain::FileVersion {
            revision: 1,
            source_version: format!("v{index}"),
        },
        presence: domain::FilePresence::Present,
        publication: domain::FilePublication::Published,
        size_bytes: Some(1024),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".into()),
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

fn library_root_id(api_state: &ApiState, library_id: &str) -> String {
    api_state
        .store()
        .lock()
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id.as_str() == library_id)
        .map(|root| root.id)
        .expect("library root")
}

fn publish_published_files(
    api_state: &ApiState,
    library_id: &str,
    media_id: MediaId,
    count: usize,
) -> Vec<LedgerId> {
    let root_id = library_root_id(api_state, library_id);
    let scope = store::library_write::WriteScope {
        library_id: library_id.parse().unwrap(),
        config_revision: 0,
    };
    let store = api_state.store();
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        let file = published_file_row(library_id, &root_id, media_id, index);
        ids.push(file.file_id);
        store
            .lock()
            .publish_files(
                &scope,
                &[store::library_write::FilePublication {
                    file: &file,
                    units: &[domain::UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                }],
                1_000,
            )
            .unwrap();
    }
    ids
}

async fn walk_file_pages(
    app: &axum::Router,
    library_id: &str,
    media_id: MediaId,
    limit: usize,
) -> (Vec<String>, Vec<Value>) {
    let mut cursor: Option<String> = None;
    let mut ids: Vec<String> = Vec::new();
    let mut rows: Vec<Value> = Vec::new();
    loop {
        let mut url =
            format!("/api/v1/libraries/{library_id}/items/{media_id}/files?limit={limit}");
        if let Some(after) = cursor.as_deref() {
            url.push_str(&format!("&after={}", urlencoding_encode(after)));
        }
        let body = get_json(app, &url).await;
        let files = body["files"].as_array().expect("files array").clone();
        assert!(
            files.len() <= limit,
            "page must not exceed limit {limit}, got {}",
            files.len()
        );
        for file in &files {
            let id = file["file_id"].as_str().unwrap().to_string();
            assert!(!ids.contains(&id), "file {id} appeared twice while paging");
            ids.push(id);
        }
        rows.extend(files);
        match body["next_cursor"].as_str() {
            Some(next) => {
                assert_eq!(
                    rows.len() % limit,
                    0,
                    "a non-final page must be exactly full"
                );
                cursor = Some(next.to_string());
            }
            None => break,
        }
    }
    (ids, rows)
}

fn urlencoding_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[tokio::test]
async fn detail_file_pages_preserve_all_files() {
    for count in [0usize, 49, 50, 51, 100, 101] {
        let tmp = tempfile::tempdir().unwrap();
        let api_state = base_state(&tmp);
        let app = router(api_state.clone());
        let (library_id, _root) = movie_library(&api_state);
        let media_id = MediaId::new();
        insert_media(&api_state, media_id, "Paged");
        let expected = publish_published_files(&api_state, &library_id, media_id, count);

        for limit in [1usize, 7, 50] {
            let (ids, rows) = walk_file_pages(&app, &library_id, media_id, limit).await;
            let mut got = ids.clone();
            let mut want: Vec<String> = expected.iter().map(|id| id.to_string()).collect();
            got.sort();
            want.sort();
            assert_eq!(
                got, want,
                "count={count} limit={limit}: page union must be exact"
            );
            for row in &rows {
                assert_eq!(row["publication"], "published");
                assert!(row["presence"].is_string());
            }
            assert_eq!(
                ids.len(),
                count,
                "count={count} limit={limit}: no duplicate and no missing file"
            );
        }
    }
}

#[tokio::test]
async fn pending_files_and_multi_unit_coverage_are_state_aware() {
    let tmp = tempfile::tempdir().unwrap();
    let api_state = base_state(&tmp);
    let app = router(api_state.clone());
    let (library_id, _root) = movie_library(&api_state);
    let media_id = MediaId::new();
    insert_media(&api_state, media_id, "Multi");
    let root_id = library_root_id(&api_state, &library_id);
    let scope = store::library_write::WriteScope {
        library_id: library_id.parse().unwrap(),
        config_revision: 0,
    };

    // One published file covering two episodes.
    let multi = published_file_row(&library_id, &root_id, media_id, 1);
    let units = [domain::UnitKey::new(1, 1), domain::UnitKey::new(1, 2)];
    api_state
        .store()
        .lock()
        .publish_files(
            &scope,
            &[store::library_write::FilePublication {
                file: &multi,
                units: &units,
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();

    // A pending reservation on the same item must stay out of browse.
    let pending = domain::FileCoverage {
        path_key: String::new(),
        file: domain::LibraryFile {
            publication: domain::FilePublication::Pending,
            path: PathBuf::from("Paged/pending.mkv"),
            ..published_file_row(&library_id, &root_id, media_id, 2)
        },
        units: vec![domain::UnitKey::new(1, 3)],
    };
    api_state
        .store()
        .lock()
        .reserve_transfer(
            &scope,
            &pending,
            Path::new("/downloads/pending.mkv"),
            library::TransferMode::Hardlink,
            2_000,
        )
        .unwrap();

    let body = get_json(
        &app,
        &format!("/api/v1/libraries/{library_id}/items/{media_id}/files?limit=50"),
    )
    .await;
    let files = body["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "pending must not leak into the browse page");
    assert_eq!(files[0]["file_id"], multi.file_id.to_string());
    assert_eq!(files[0]["publication"], "published");
    let units_json = files[0]["units"].as_array().expect("units array");
    assert_eq!(
        units_json.len(),
        2,
        "a multi-unit file carries complete coverage"
    );
    assert_eq!(units_json[0]["season"], 1);
    assert_eq!(units_json[0]["episode"], 1);
    assert_eq!(units_json[1]["episode"], 2);
    assert_eq!(
        body["next_cursor"],
        Value::Null,
        "one file is the last page"
    );
}
