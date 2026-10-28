//! `GET /libraries/{id}/items` 与 `/gallery` 的显式分页校验。
//!
//! 越界的显式 `limit` 必须 400，不再静默 clamp；`offset` 已删除（改用
//! `cursor`，见 library_cursor_paging.rs），任何取值一律 400；省略 `limit`
//! 时按默认上限返回一页。

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
        .lock()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.kind == MediaKind::Movie)
        .expect("default movie library");
    (library.id, library.root_paths[0].clone())
}

fn seed_movie(state: &ApiState, title: &str, root: &Path, with_poster: bool) {
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
    if with_poster {
        std::fs::write(dir.join("poster.jpg"), b"poster").unwrap();
    }
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

fn fixture() -> (tempfile::TempDir, axum::Router, String) {
    let tmp = tempfile::tempdir().unwrap();
    let api_state = base_state(&tmp);
    let (library_id, root) = movie_library(&api_state);
    for index in 0..5 {
        // 只有前两个带海报，用来验证 gallery 的"先剔除空图组再分页"。
        seed_movie(&api_state, &format!("Movie {index}"), &root, index < 2);
    }
    (tmp, router(api_state), library_id)
}

#[tokio::test]
async fn invalid_explicit_paging_is_rejected_on_both_paths() {
    let (_tmp, app, library_id) = fixture();
    for bad in ["limit=0", "limit=501", "limit=-1", "limit=abc", "limit="] {
        let (status, body) =
            get(&app, &format!("/api/v1/libraries/{library_id}/items?{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "items {bad}: {body}");
        assert_eq!(body["error"]["code"], "library.invalid");
        let (status, body) = get(
            &app,
            &format!("/api/v1/libraries/{library_id}/gallery?{bad}"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "gallery {bad}: {body}");
    }
    // offset 整个参数已删除：合法取值也一律 400（不留兼容分支）。
    for bad in [
        "offset=-1",
        "offset=abc",
        "offset=",
        "offset=0",
        "offset=2",
        "offset=18446744073709551615",
    ] {
        let (status, _) = get(&app, &format!("/api/v1/libraries/{library_id}/items?{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "items {bad}");
        let (status, _) = get(
            &app,
            &format!("/api/v1/libraries/{library_id}/gallery?{bad}"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "gallery {bad}");
    }
}

#[tokio::test]
async fn huge_offset_is_rejected_instead_of_overflowing() {
    let (_tmp, app, library_id) = fixture();
    for path in ["items", "gallery"] {
        let (status, body) = get(
            &app,
            &format!(
                "/api/v1/libraries/{library_id}/{path}?offset={}&limit=10",
                usize::MAX
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{path}: offset 已删除，任何取值都要 400: {body}"
        );
    }
}

#[tokio::test]
async fn explicit_limit_returns_the_requested_page() {
    let (_tmp, app, library_id) = fixture();
    let (status, body) = get(
        &app,
        &format!("/api/v1/libraries/{library_id}/items?limit=200"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"].as_array().unwrap().len(), 5);

    let (status, body) = get(
        &app,
        &format!("/api/v1/libraries/{library_id}/items?limit=1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"].as_array().unwrap().len(), 1);

    // 第二页走游标：第一页的 next_cursor 原样带回来。
    let (status, first) = get(
        &app,
        &format!("/api/v1/libraries/{library_id}/items?limit=2"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("第一页应给出 next_cursor");
    assert_eq!(first["total"].as_u64(), Some(5), "第一页带总数");
    let (status, second) = get(
        &app,
        &format!(
            "/api/v1/libraries/{library_id}/items?limit=2&cursor={}",
            urlencode(cursor)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["data"].as_array().unwrap().len(), 2);
    assert!(
        second["total"].is_null(),
        "续页不重复算总数，total 必须是 null"
    );
    let first_ids: Vec<&str> = first["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["media_item_id"].as_str().unwrap())
        .collect();
    let second_ids: Vec<&str> = second["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["media_item_id"].as_str().unwrap())
        .collect();
    assert!(
        first_ids.iter().all(|id| !second_ids.contains(id)),
        "两页不能重叠"
    );
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

#[tokio::test]
async fn omitted_limit_returns_the_default_page() {
    let (_tmp, app, library_id) = fixture();
    let (status, body) = get(&app, &format!("/api/v1/libraries/{library_id}/items")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"].as_array().unwrap().len(),
        5,
        "默认页上限大于本 fixture 的条目数，应全部返回"
    );

    // gallery：5 个 Media 里只有 2 个有图，空图组先被剔除，再分页。
    let (status, body) = get(&app, &format!("/api/v1/libraries/{library_id}/gallery")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"].as_array().unwrap().len(), 2);

    // gallery 的分页发生在剔除空图组之后：limit=1 拿第一组，游标拿第二组。
    let (status, first) = get(
        &app,
        &format!("/api/v1/libraries/{library_id}/gallery?limit=1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["data"].as_array().unwrap().len(), 1);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("还有下一组时应给出 next_cursor");
    let (status, second) = get(
        &app,
        &format!(
            "/api/v1/libraries/{library_id}/gallery?limit=5&cursor={}",
            urlencode(cursor)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        second["data"].as_array().unwrap().len(),
        1,
        "第二页只剩那一组"
    );
}

/// A JSON-shaped but useless cursor reaches the store's cursor decoder. That
/// rejection is a client error: it must be the same 400 `library.invalid` the
/// handler gives a non-JSON cursor, never a 500 (`library.read`) — a 500 for bad
/// input pollutes the error budget and hides the real ones.
#[tokio::test]
async fn malformed_json_cursor_is_a_client_error() {
    let (_tmp, app, library_id) = fixture();
    for cursor in ["{}", r#"{"keys":[]}"#] {
        let (status, body) = get(
            &app,
            &format!(
                "/api/v1/libraries/{library_id}/items?cursor={}",
                urlencode(cursor)
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "cursor={cursor} must be a client error: {body}"
        );
        assert_eq!(body["error"]["code"], "library.invalid", "{body}");
    }

    // A non-JSON cursor never reaches a read: the handler rejects it first.
    let (status, body) = get(
        &app,
        &format!("/api/v1/libraries/{library_id}/items?cursor=not-a-cursor"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "library.invalid", "{body}");
}

#[tokio::test]
async fn browse_uses_only_current_watch_and_sort_values() {
    let (_tmp, app, library_id) = fixture();
    for path in ["items", "gallery", "facets", "item-index"] {
        for watch in ["watched", "unplayed", "unseen"] {
            let (status, body) = get(
                &app,
                &format!("/api/v1/libraries/{library_id}/{path}?w={watch}"),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path} {watch}: {body}");
            assert_eq!(body["error"]["code"], "library.invalid");
        }
        for watch in ["unwatched", "watching", "played", "favorite", "seen"] {
            let (status, body) = get(
                &app,
                &format!("/api/v1/libraries/{library_id}/{path}?w={watch}"),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{path} {watch}: {body}");
        }
        for sort in ["release_date_asc", "nonsense"] {
            let (status, body) = get(
                &app,
                &format!("/api/v1/libraries/{library_id}/{path}?sort={sort}"),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path} {sort}: {body}");
        }
        let (status, body) = get(
            &app,
            &format!("/api/v1/libraries/{library_id}/{path}?sort=release_date&order=asc"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }
}
