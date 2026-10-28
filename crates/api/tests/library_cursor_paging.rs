//! D5：浏览列表改用键集游标（`cursor`），`offset` 参数删除。
//!
//! 同一深度下 offset 分页要按 offset 逐行跳过、每页都可能带 `COUNT`；键集游标
//! 是常数代价。这里锁三件事：
//!   1. 250 条的库按 50 条一页用 `next_cursor` 翻完，每个条目恰好出现一次；
//!   2. `total` 只在第一页（没有 cursor 的那页）出现，续页为 `null`；
//!   3. `offset` 一律 400（不留兼容分支），坏游标也是 400。

use std::collections::{HashMap, HashSet};
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

const PAGE_SIZE: usize = 50;
const ITEMS: usize = 250;

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
        .lock()
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
    state.store().lock().insert_media(&media).unwrap();
    let dir = root.join(title);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{title}.mkv"));
    std::fs::write(&file, b"video").unwrap();
    std::fs::write(dir.join("poster.jpg"), b"poster").unwrap();
    publish_fixture_file(
        &state.store().lock(),
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

/// 一页的 (条目, next_cursor, total)。
fn page(body: &Value) -> (Vec<Value>, Option<String>, Option<u64>) {
    let items = body["data"]
        .as_array()
        .unwrap_or_else(|| panic!("data 必须是数组: {body}"))
        .clone();
    let next = body["next_cursor"].as_str().map(str::to_string);
    let total = body["total"].as_u64();
    (items, next, total)
}

fn fixture(items: usize) -> (tempfile::TempDir, axum::Router, String) {
    let tmp = tempfile::tempdir().unwrap();
    let api_state = base_state(&tmp);
    let (library_id, root) = movie_library(&api_state);
    for index in 0..items {
        seed_movie(&api_state, &format!("Movie {index:03}"), &root);
    }
    (tmp, router(api_state), library_id)
}

#[tokio::test]
async fn cursor_pages_cover_every_item_exactly_once() {
    let (_tmp, app, library_id) = fixture(ITEMS);
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let url = match &cursor {
            None => format!("/api/v1/libraries/{library_id}/items?limit={PAGE_SIZE}"),
            Some(raw) => format!(
                "/api/v1/libraries/{library_id}/items?limit={PAGE_SIZE}&cursor={}",
                urlencode(raw)
            ),
        };
        let (status, body) = get(&app, &url).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (items, next, total) = page(&body);
        pages += 1;
        if cursor.is_none() {
            assert_eq!(
                total,
                Some(ITEMS as u64),
                "第一页要带总数，供「共 N 部」使用"
            );
            assert_eq!(items.len(), PAGE_SIZE);
        } else {
            assert!(
                total.is_none(),
                "续页不算 COUNT：total 必须是 null，第 {pages} 页是 {total:?}"
            );
        }
        seen.extend(
            items
                .iter()
                .map(|item| item["media_item_id"].as_str().unwrap().to_string()),
        );
        match next {
            Some(raw) => cursor = Some(raw),
            None => break,
        }
        assert!(pages <= ITEMS, "游标没有收敛：{pages} 页仍在续");
    }
    assert_eq!(pages, ITEMS / PAGE_SIZE, "250 条 / 每页 50 = 5 页");
    let unique: HashSet<&String> = seen.iter().collect();
    assert_eq!(unique.len(), ITEMS, "每个条目必须恰好出现一次");
}

#[tokio::test]
async fn gallery_cursor_pages_cover_every_group_once() {
    let (_tmp, app, library_id) = fixture(6);
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let url = match &cursor {
            None => format!("/api/v1/libraries/{library_id}/gallery?limit=2"),
            Some(raw) => format!(
                "/api/v1/libraries/{library_id}/gallery?limit=2&cursor={}",
                urlencode(raw)
            ),
        };
        let (status, body) = get(&app, &url).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (groups, next, total) = page(&body);
        assert!(total.is_none(), "图廊不报总数");
        seen.extend(
            groups
                .iter()
                .map(|group| group["media_item_id"].as_str().unwrap().to_string()),
        );
        match next {
            Some(raw) => cursor = Some(raw),
            None => break,
        }
        assert!(seen.len() <= 6, "图廊游标没有收敛");
    }
    let unique: HashSet<&String> = seen.iter().collect();
    assert_eq!(unique.len(), 6, "6 部作品各自出现一次");
}

#[tokio::test]
async fn offset_param_is_rejected_on_both_paths() {
    let (_tmp, app, library_id) = fixture(3);
    for bad in [
        "offset=0",
        "offset=50",
        "offset=-1",
        "offset=abc",
        "offset=",
    ] {
        for path in ["items", "gallery"] {
            let (status, body) = get(
                &app,
                &format!("/api/v1/libraries/{library_id}/{path}?{bad}"),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "{path} {bad} 必须 400，而不是静默接受: {body}"
            );
            assert_eq!(body["error"]["code"], "library.invalid");
        }
    }
}

#[tokio::test]
async fn malformed_cursor_is_rejected() {
    let (_tmp, app, library_id) = fixture(3);
    // 游标是服务端签发的 JSON：截断/误编码的游标要在进读路径之前就 400。
    for bad in ["not-a-cursor", "%7B", "abc%00"] {
        for path in ["items", "gallery"] {
            let (status, body) = get(
                &app,
                &format!("/api/v1/libraries/{library_id}/{path}?cursor={bad}"),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "{path} cursor={bad}: {body}"
            );
            assert_eq!(body["error"]["code"], "library.invalid");
        }
    }
}

#[tokio::test]
async fn cursor_from_different_order_is_a_client_error() {
    let (_tmp, app, library_id) = fixture(3);
    let (_, first) = get(
        &app,
        &format!("/api/v1/libraries/{library_id}/items?limit=1&sort=title"),
    )
    .await;
    let cursor = first["next_cursor"].as_str().unwrap();
    let (status, body) = get(
        &app,
        &format!(
            "/api/v1/libraries/{library_id}/items?limit=1&sort=rating&cursor={}",
            urlencode(cursor)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "library.invalid");
}

#[tokio::test]
async fn limit_is_still_validated() {
    let (_tmp, app, library_id) = fixture(2);
    for bad in ["limit=0", "limit=501", "limit=abc"] {
        let (status, body) =
            get(&app, &format!("/api/v1/libraries/{library_id}/items?{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let (status, body) = get(&app, &format!("/api/v1/libraries/{library_id}/items")).await;
    assert_eq!(status, StatusCode::OK);
    let (items, next, _) = page(&body);
    assert_eq!(items.len(), 2, "省略 limit 时按默认上限返回一页");
    assert!(next.is_none(), "条目不足一页时没有下一个游标");
}

/// 游标是 JSON，必须整体转义后再拼进查询串。
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
