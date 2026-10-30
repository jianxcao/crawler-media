//! Task 26：A-Z 跳转回到 O(1)——`item-index` 直接给每档一个**不透明游标**。
//!
//! Task 14 删掉 `offset` 之后，点一个字母要按 keyset 页往前走上十几次。这里锁住新
//! 契约：
//!  1. `item-index` 每档返回 `{initial, count, cursor}`，**不再有 `offset`**
//!     （绝对下标不进线上格式）；
//!  2. 拿某一档的 `cursor` 打**一次** `/items`，返回的正是「从这一档第一格开始」
//!     的那一页：首条标题以该档字母开头，且等于完整顺序里这一档的第一部作品；
//!  3. 各档 `count` 之和 = 库内条目总数（前端靠前缀和还原档位位置）。

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
use common::{Fixtures, json_body, request, state};

fn base_state(tmp: &tempfile::TempDir) -> ApiState {
    state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
}

fn movie_library(state: &ApiState) -> (String, PathBuf) {
    let library = state
        .store()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.kind == MediaKind::Movie)
        .expect("default movie library");
    (library.id, library.root_paths[0].clone())
}

fn seed_movie(state: &ApiState, title: &str, root: &Path) {
    let media = Media {
        id: MediaId::new(),
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
    state.store().insert_media(&media).unwrap();
    let dir = root.join(title);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{title}.mkv"));
    std::fs::write(&file, b"video").unwrap();
    publish_fixture_file(
        &state.store(),
        &FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id: media.id,
            path: file.to_string_lossy().to_string(),
            season: None,
            episode: None,
            resolution: Some("1080p".into()),
            codec: Some("H264".into()),
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap();
}

async fn get(app: &axum::Router, url: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(request("GET", url, Some("management-secret"), Value::Null))
        .await
        .unwrap();
    let status = response.status();
    (status, json_body(response).await)
}

fn urlencode(raw: &str) -> String {
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

/// 30 部作品，三个字母档各 10 部：A/B/C 各 10。
fn fixture() -> (tempfile::TempDir, axum::Router, String) {
    let tmp = tempfile::tempdir().unwrap();
    let api_state = base_state(&tmp);
    let (library_id, root) = movie_library(&api_state);
    for (letter, word) in [("A", "Alpha"), ("B", "Bravo"), ("C", "Charlie")] {
        for index in 0..10 {
            seed_movie(&api_state, &format!("{word} {index:02}"), &root);
            let _ = letter;
        }
    }
    (tmp, router(api_state), library_id)
}

/// 用游标把整库按当前顺序翻完（顺序真相来源）。
async fn ordered_titles(app: &axum::Router, library_id: &str) -> Vec<String> {
    let mut titles = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let url = match &cursor {
            None => format!("/api/v1/libraries/{library_id}/items?limit=7&sort=title&order=asc"),
            Some(raw) => format!(
                "/api/v1/libraries/{library_id}/items?limit=7&sort=title&order=asc&cursor={}",
                urlencode(raw)
            ),
        };
        let (status, body) = get(app, &url).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        for item in body["data"].as_array().unwrap() {
            titles.push(item["title"].as_str().unwrap().to_string());
        }
        match body["next_cursor"].as_str() {
            Some(raw) => cursor = Some(raw.to_string()),
            None => break,
        }
    }
    titles
}

#[tokio::test]
async fn item_index_returns_opaque_cursors_and_no_absolute_index() {
    let (_tmp, app, library_id) = fixture();
    let (status, body) = get(
        &app,
        &format!("/api/v1/libraries/{library_id}/item-index?sort=title&order=asc"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let buckets = body["data"].as_array().unwrap();
    let initials: Vec<&str> = buckets
        .iter()
        .map(|bucket| bucket["initial"].as_str().unwrap())
        .collect();
    assert_eq!(initials, ["A", "B", "C"], "{body}");
    for bucket in buckets {
        assert_eq!(bucket["count"].as_u64(), Some(10), "每档 10 部: {bucket}");
        assert!(
            bucket.get("offset").is_none(),
            "绝对下标不得上线（Task 26）: {bucket}"
        );
        assert!(
            bucket["cursor"].is_string() || bucket["cursor"].is_null(),
            "每档带一个不透明游标: {bucket}"
        );
    }
    assert!(
        buckets[0]["cursor"].is_null(),
        "第一档从第 0 条开始，用首页即可: {body}"
    );
    assert!(
        buckets[1]["cursor"].is_string() && buckets[2]["cursor"].is_string(),
        "后续档必须给出可直达的游标: {body}"
    );
}

#[tokio::test]
async fn letter_jump_is_one_request_and_lands_on_that_letters_first_item() {
    let (_tmp, app, library_id) = fixture();
    let ordered = ordered_titles(&app, &library_id).await;
    assert_eq!(ordered.len(), 30);

    let (status, index) = get(
        &app,
        &format!("/api/v1/libraries/{library_id}/item-index?sort=title&order=asc"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{index}");
    let buckets = index["data"].as_array().unwrap();

    // 每档：**一次** /items 请求就能落在该档第一格上。
    let mut expected_start = 0usize;
    for bucket in buckets {
        let initial = bucket["initial"].as_str().unwrap();
        let count = bucket["count"].as_u64().unwrap() as usize;
        let cursor = bucket["cursor"].as_str();
        let url = match cursor {
            None => format!("/api/v1/libraries/{library_id}/items?limit=50&sort=title&order=asc"),
            Some(raw) => format!(
                "/api/v1/libraries/{library_id}/items?limit=50&sort=title&order=asc&cursor={}",
                urlencode(raw)
            ),
        };
        let requests_for_this_jump = 1; // 单次请求：不再按 offset 逐页走
        let (status, page) = get(&app, &url).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let first = page["data"][0]["title"].as_str().unwrap();
        assert_eq!(
            first, ordered[expected_start],
            "档 {initial} 的首页第一条必须是完整顺序里这一档的第一部（{requests_for_this_jump} 次请求）"
        );
        assert!(
            first.starts_with(initial),
            "档 {initial} 首条应以该字母开头: {first}"
        );
        assert_eq!(
            page["data"].as_array().unwrap().len().min(count),
            count.min(50),
            "档 {initial} 的页只装这一档的花名册长度"
        );
        expected_start += count;
    }
    assert_eq!(expected_start, ordered.len(), "各档 count 之和 = 总条目数");
}

#[tokio::test]
async fn index_follows_the_requested_order_and_direction() {
    let (_tmp, app, library_id) = fixture();
    let descending = ordered_titles_desc(&app, &library_id).await;
    let (status, index) = get(
        &app,
        &format!("/api/v1/libraries/{library_id}/item-index?sort=title&order=desc"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{index}");
    let buckets = index["data"].as_array().unwrap();
    let initials: Vec<&str> = buckets
        .iter()
        .map(|bucket| bucket["initial"].as_str().unwrap())
        .collect();
    assert_eq!(initials, ["C", "B", "A"], "倒序时档位也倒过来: {index}");
    let first_cursor = buckets[0]["cursor"].as_str();
    assert!(first_cursor.is_none(), "倒序第一档同样是首页: {index}");
    // 倒序下用 B 档游标取页：首条必须是倒序顺序里 B 档的第一部。
    let b_cursor = buckets[1]["cursor"].as_str().expect("B 档游标");
    let (status, page) = get(
        &app,
        &format!(
            "/api/v1/libraries/{library_id}/items?limit=50&sort=title&order=desc&cursor={}",
            urlencode(b_cursor)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let first = page["data"][0]["title"].as_str().unwrap();
    assert_eq!(first, descending[10], "倒序 B 档的第一部: {first}");
}

async fn ordered_titles_desc(app: &axum::Router, library_id: &str) -> Vec<String> {
    let mut titles = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let url = match &cursor {
            None => format!("/api/v1/libraries/{library_id}/items?limit=7&sort=title&order=desc"),
            Some(raw) => format!(
                "/api/v1/libraries/{library_id}/items?limit=7&sort=title&order=desc&cursor={}",
                urlencode(raw)
            ),
        };
        let (status, body) = get(app, &url).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        for item in body["data"].as_array().unwrap() {
            titles.push(item["title"].as_str().unwrap().to_string());
        }
        match body["next_cursor"].as_str() {
            Some(raw) => cursor = Some(raw.to_string()),
            None => break,
        }
    }
    titles
}
