//! Actual HTTP cursor/position/gallery and selected-page SQL work contracts.
#[allow(dead_code)]
#[path = "playback_read_regression/common.rs"]
mod common;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use common::{add_ledger, favorite, make_state, seed_library, seed_media, seed_member};
use media_server::{
    provider::QueryTiming,
    query_metrics::{QueryMetrics, QueryMetricsSnapshot},
};
use serde_json::Value;
use tower::ServiceExt;

struct Fixture {
    _dir: tempfile::TempDir,
    state: api::ApiState,
    user: domain::UserId,
    token: String,
    root: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let state = make_state(dir.path());
        let root = seed_library(&state.store(), dir.path(), "cursor");
        let (user, token) = seed_member(&state.store(), "cursor-member");
        Self {
            _dir: dir,
            state,
            user,
            token,
            root,
        }
    }
    fn add(&self, n: usize) -> String {
        let store = self.state.store();
        let title = format!("Movie{n:03}");
        let media = seed_media(&store, &title, Some(2024));
        add_ledger(&store, media, &self.root, &title);
        favorite(&store, self.user, media, true);
        media.to_string()
    }
    async fn get(&self, path: &str) -> (StatusCode, Value, QueryMetricsSnapshot) {
        let timing = QueryTiming::start();
        let metrics = QueryMetrics::new(timing, true);
        let mut request = Request::get(path)
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(timing);
        request.extensions_mut().insert(metrics.clone());
        let response = api::router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap(),
            metrics.snapshot(),
        )
    }
}
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}
fn ids(body: &Value) -> Vec<String> {
    body["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["media_item_id"].as_str().unwrap().to_owned())
        .collect()
}
const SCOPE: &str = "sort=title&order=asc";

#[tokio::test]
async fn actual_favorites_cursor_pages_and_gallery_share_selected_media_and_head_only_total() {
    let fixture = Fixture::new();
    let expected = (0..25).map(|n| fixture.add(n)).collect::<Vec<_>>();
    let mut after = None;
    let mut seen = Vec::new();
    for (index, size) in [10, 10, 5].into_iter().enumerate() {
        let suffix = after
            .as_ref()
            .map(|cursor: &String| format!("&after={}", encode(cursor)))
            .unwrap_or_default();
        let path = format!("/api/v1/playback/favorites?limit=10&{SCOPE}{suffix}");
        let (status, body, _) = fixture.get(&path).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let selected = ids(&body);
        assert_eq!(selected.len(), size);
        assert_eq!(
            body["data"]["total"],
            if index == 0 {
                serde_json::json!(25)
            } else {
                Value::Null
            }
        );
        let (status, gallery, _) = fixture
            .get(&format!(
                "/api/v1/playback/favorites/gallery?limit=10&{SCOPE}{suffix}"
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{gallery}");
        assert_eq!(
            ids(&gallery),
            selected,
            "image count must not alter selected Media positions"
        );
        assert!(
            gallery["data"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["images"].as_array().unwrap().is_empty())
        );
        seen.extend(selected);
        after = body["data"]["next_cursor"].as_str().map(str::to_owned);
    }
    assert_eq!(after, None);
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn actual_favorite_position_seek_keeps_jump_and_gallery_positions_without_offset() {
    let fixture = Fixture::new();
    let expected = (0..25).map(|n| fixture.add(n)).collect::<Vec<_>>();
    let (status, position, _) = fixture
        .get(&format!(
            "/api/v1/playback/favorites/position?start=13&{SCOPE}"
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{position}");
    assert_eq!(position["data"]["start"], 13);
    let after = position["data"]["after"].as_str().unwrap();
    for endpoint in ["favorites", "favorites/gallery"] {
        let (status, page, _) = fixture
            .get(&format!(
                "/api/v1/playback/{endpoint}?limit=5&{SCOPE}&after={}",
                encode(after)
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert_eq!(ids(&page), expected[13..18]);
        assert_eq!(page["data"]["total"], Value::Null);
    }
    let (status, end, _) = fixture
        .get(&format!(
            "/api/v1/playback/favorites/position?start=999&{SCOPE}"
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{end}");
    assert_eq!(end["data"]["start"], 999);
    let boundary = end["data"]["after"].as_str().unwrap();
    let (status, terminal, _) = fixture
        .get(&format!(
            "/api/v1/playback/favorites?limit=5&{SCOPE}&after={}",
            encode(boundary)
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{terminal}");
    assert!(ids(&terminal).is_empty());
    assert_eq!(terminal["data"]["total"], Value::Null);
    assert_eq!(terminal["data"]["next_cursor"], Value::Null);
    let (status, _, _) = fixture
        .get("/api/v1/playback/favorites/position?start=invalid")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn actual_favorites_reject_obsolete_offsets_and_malformed_or_cross_sort_cursors() {
    let mut fixture = Fixture::new();
    for n in 0..3 {
        fixture.add(n);
    }
    for suffix in [
        "?offset=0",
        "?sort=rating",
        "/gallery?sort=size",
        "/gallery?offset=1",
        "?after=invalid",
        "/gallery?after=invalid",
        "/position?start=1&after=invalid",
    ] {
        let (status, _, _) = fixture
            .get(&format!("/api/v1/playback/favorites{suffix}"))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{suffix}");
    }
    let (_, head, _) = fixture
        .get(&format!("/api/v1/playback/favorites?limit=1&{SCOPE}"))
        .await;
    let after = encode(head["data"]["next_cursor"].as_str().unwrap());
    let (status, _, _) = fixture
        .get(&format!(
            "/api/v1/playback/favorites?limit=1&sort=title&order=desc&after={after}"
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, other_token) = seed_member(&fixture.state.store(), "other-cursor-member");
    fixture.token = other_token;
    let (status, _, _) = fixture
        .get(&format!(
            "/api/v1/playback/favorites?limit=1&{SCOPE}&after={after}"
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "cursor must bind its user scope"
    );
}

#[tokio::test]
async fn actual_favorites_selected_page_hydration_has_constant_statements() {
    let fixture = Fixture::new();
    for n in 0..5 {
        fixture.add(n);
    }
    let (_, small_body, small) = fixture.get("/api/v1/playback/favorites?limit=50").await;
    assert_eq!(ids(&small_body).len(), 5);
    for n in 5..50 {
        fixture.add(n);
    }
    let (_, large_body, large) = fixture.get("/api/v1/playback/favorites?limit=50").await;
    assert_eq!(ids(&large_body).len(), 50);
    assert!(
        small.statements.is_some_and(|count| count > 0),
        "measurement must be real: {small:?}"
    );
    assert_eq!(
        small.statements, large.statements,
        "selected page N+1: {small:?}->{large:?}"
    );
}
