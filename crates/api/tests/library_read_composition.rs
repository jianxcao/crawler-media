//! Complete protocol responses retain one authorization attempt through all segments.
use api::{ApiState, Store, router};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::provider::QueryTiming;
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tower::ServiceExt;

const TOKEN: &str = "management-secret";
struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}
struct Fixture {
    dir: tempfile::TempDir,
    state: ApiState,
    a: String,
    b: String,
    a_item: String,
    b_item: String,
}
impl Fixture {
    fn new(populate: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let state = ApiState::new(
            store,
            TOKEN.into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        let (a, b, a_item, b_item) = {
            let store = state.store();
            let store = store.lock();
            let root_a = dir.path().join("a");
            let root_b = dir.path().join("b");
            std::fs::create_dir_all(&root_a).unwrap();
            std::fs::create_dir_all(&root_b).unwrap();
            let a = store
                .create_library(
                    MediaKind::Movie,
                    "A",
                    &[root_a.to_str().unwrap()],
                    "members",
                    true,
                    &[],
                )
                .unwrap()
                .id;
            let b = store
                .create_library(
                    MediaKind::Movie,
                    "B",
                    &[root_b.to_str().unwrap()],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap()
                .id;
            let a_media = insert_movie(&store, root_a.join("a.mkv"), "A", populate);
            let b_media = insert_movie(&store, root_b.join("b.mkv"), "B", populate);
            let user = store.user_by_token(TOKEN).unwrap().unwrap().id;
            if populate {
                store
                    .upsert_unit(
                        user,
                        b_media,
                        -1,
                        -1,
                        0,
                        Some(true),
                        None,
                        None,
                        None,
                        None,
                        false,
                        1,
                    )
                    .unwrap();
            }
            (
                a,
                b,
                domain::TypedItemId::Work(a_media).to_wire_string(),
                domain::TypedItemId::Work(b_media).to_wire_string(),
            )
        };
        Self {
            dir,
            state,
            a,
            b,
            a_item,
            b_item,
        }
    }
    fn library_db(&self) -> std::path::PathBuf {
        self.dir.path().join("data/library.db")
    }
    fn app_db(&self) -> std::path::PathBuf {
        self.dir.path().join("data/app.db")
    }
}
fn insert_movie(store: &Store, path: std::path::PathBuf, name: &str, populate: bool) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: name.into(),
        original_title: None,
        aliases: vec![],
        year: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    if populate {
        std::fs::write(&path, b"fixture").unwrap();
        store::test_support::publish_fixture_file(
            store,
            &store::test_support::FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
    }
    media.id
}
fn hide(conn: &rusqlite::Connection, library: &str) {
    assert_eq!(
        conn.execute(
            "UPDATE libraries SET admin_visible=0 WHERE library_id=?1",
            [library]
        )
        .unwrap(),
        1
    );
}
async fn get(state: ApiState, uri: &str, timing: Option<QueryTiming>) -> (StatusCode, Value) {
    let mut request = Request::get(uri)
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("X-Emby-Device-Id", "composition-device")
        .body(Body::empty())
        .unwrap();
    if let Some(timing) = timing {
        request.extensions_mut().insert(timing);
    }
    let response = router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
fn ids(items: &Value) -> Vec<String> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["Id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn latest_acl_change_during_second_segment_recomputes_complete_response() {
    for prefix in ["", "/emby"] {
        let mut fixture = Fixture::new(true);
        let db = fixture.library_db();
        let a = fixture.a.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        fixture.state = fixture
            .state
            .with_protocol_selection_barrier(Arc::new(move |_| {
                if observed.fetch_add(1, Ordering::SeqCst) == 1 {
                    hide(&rusqlite::Connection::open(&db).unwrap(), &a);
                }
            }));
        let (status, body) = get(
            fixture.state,
            &format!("{prefix}/Users/me/Items/Latest?Limit=2"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(ids(&body), [fixture.b_item]);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "both segments must be recomputed"
        );
    }
}

#[tokio::test]
async fn latest_role_change_during_second_segment_discards_earlier_item() {
    let mut fixture = Fixture::new(true);
    let db = fixture.app_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    fixture.state = fixture.state.with_protocol_selection_barrier(Arc::new(move |_| {
        if observed.fetch_add(1, Ordering::SeqCst) == 1 {
            rusqlite::Connection::open(&db).unwrap().execute(
                "UPDATE users SET role='member' WHERE id=(SELECT user_id FROM user_tokens WHERE token=?1)", [TOKEN]).unwrap();
        }
    }));
    let (status, body) = get(fixture.state, "/Users/me/Items/Latest?Limit=2", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), [fixture.b_item]);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn latest_second_segment_token_revocation_rejects_entire_response() {
    let mut fixture = Fixture::new(true);
    let db = fixture.app_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    fixture.state = fixture
        .state
        .with_protocol_selection_barrier(Arc::new(move |_| {
            if observed.fetch_add(1, Ordering::SeqCst) == 1 {
                rusqlite::Connection::open(&db)
                    .unwrap()
                    .execute("DELETE FROM user_tokens WHERE token=?1", [TOKEN])
                    .unwrap();
            }
        }));
    let (status, _) = get(fixture.state, "/emby/Users/me/Items/Latest?Limit=2", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn latest_two_changes_reject_without_third_complete_attempt() {
    let mut fixture = Fixture::new(true);
    let db = fixture.library_db();
    let (a, b) = (fixture.a.clone(), fixture.b.clone());
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    fixture.state = fixture
        .state
        .with_protocol_selection_barrier(Arc::new(move |_| {
            let call = observed.fetch_add(1, Ordering::SeqCst);
            let conn = rusqlite::Connection::open(&db).unwrap();
            if call == 1 {
                hide(&conn, &a);
            }
            if call == 3 {
                conn.execute(
                    "UPDATE libraries SET display_locale='en-US' WHERE library_id=?1",
                    [&b],
                )
                .unwrap();
            }
        }));
    let (status, _) = get(fixture.state, "/Users/me/Items/Latest?Limit=2", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn latest_complete_retry_uses_original_deadline_remainder() {
    let mut fixture = Fixture::new(true);
    let db = fixture.library_db();
    let a = fixture.a.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(5);
    fixture.state = fixture
        .state
        .with_protocol_selection_barrier(Arc::new(move |_| {
            let call = observed.fetch_add(1, Ordering::SeqCst);
            if call == 1 {
                hide(&rusqlite::Connection::open(&db).unwrap(), &a);
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .saturating_sub(Duration::from_secs(2)),
                );
            }
            if call == 3 {
                std::thread::sleep(
                    deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(50),
                );
            }
        }));
    let timing = QueryTiming { started, deadline };
    let (status, _) = get(
        fixture.state,
        "/Users/me/Items/Latest?Limit=2",
        Some(timing),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn root_selected_acl_change_retries_current_roster_with_exact_total() {
    for prefix in ["", "/emby"] {
        let mut fixture = Fixture::new(true);
        let db = fixture.library_db();
        let a = fixture.a.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        fixture.state = fixture
            .state
            .with_protocol_selection_barrier(Arc::new(move |_| {
                if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                    hide(&rusqlite::Connection::open(&db).unwrap(), &a);
                }
            }));
        let (status, body) = get(fixture.state, &format!("{prefix}/Items?Limit=2"), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["TotalRecordCount"], 1);
        assert_eq!(ids(&body["Items"]), [fixture.b_item]);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn root_second_context_change_returns_503_without_third_attempt() {
    let mut fixture = Fixture::new(true);
    let db = fixture.library_db();
    let (a, b) = (fixture.a.clone(), fixture.b.clone());
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    fixture.state = fixture
        .state
        .with_protocol_selection_barrier(Arc::new(move |_| {
            let call = observed.fetch_add(1, Ordering::SeqCst);
            let conn = rusqlite::Connection::open(&db).unwrap();
            if call == 0 {
                hide(&conn, &a);
            }
            if call == 1 {
                conn.execute(
                    "UPDATE libraries SET display_locale='en-US' WHERE library_id=?1",
                    [&b],
                )
                .unwrap();
            }
        }));
    let (status, _) = get(fixture.state, "/Items?Limit=2", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn resolved_explicit_library_lost_before_prepare_never_becomes_empty_success() {
    for populate in [false, true] {
        let mut fixture = Fixture::new(populate);
        let db = fixture.library_db();
        let a = fixture.a.clone();
        let metrics = fixture.state.clone();
        let fired = Arc::new(AtomicBool::new(false));
        let observed = fired.clone();
        fixture.state = fixture
            .state
            .with_library_read_stage_barrier(Arc::new(move |_| {
                if metrics.query_phase_snapshot().selection_ns > 0
                    && !observed.swap(true, Ordering::SeqCst)
                {
                    hide(&rusqlite::Connection::open(&db).unwrap(), &a);
                }
            }));
        let (status, _) = get(
            fixture.state,
            &format!("/Items?ParentId={}&Limit=2", fixture.a),
            None,
        )
        .await;
        assert!(
            fired.load(Ordering::SeqCst),
            "race must occur after resolution"
        );
        assert_eq!(status, StatusCode::UNAUTHORIZED, "populated={populate}");
    }
}

#[tokio::test]
async fn initially_unknown_parent_remains_empty_success() {
    let fixture = Fixture::new(true);
    let (status, body) = get(
        fixture.state,
        &format!("/Items?ParentId={}&Limit=2", uuid::Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["TotalRecordCount"], 0);
    assert!(ids(&body["Items"]).is_empty());
}

#[tokio::test]
async fn latest_preserves_unplayed_first_order_and_combined_offsets() {
    let fixture = Fixture::new(true);
    let (status, body) = get(
        fixture.state.clone(),
        "/Users/me/Items/Latest?Limit=2",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&body), [fixture.a_item, fixture.b_item.clone()]);
    let (status, body) = get(
        fixture.state,
        "/emby/Users/me/Items/Latest?Limit=2&StartIndex=1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&body), [fixture.b_item]);
}

#[tokio::test]
async fn explicit_library_identity_survives_context_retry_after_role_loss() {
    for populate in [false, true] {
        let mut fixture = Fixture::new(populate);
        let db = fixture.app_db();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        fixture.state = fixture.state.with_protocol_selection_barrier(Arc::new(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            rusqlite::Connection::open(&db).unwrap().execute(
                "UPDATE users SET role='member' WHERE id=(SELECT user_id FROM user_tokens WHERE token=?1)", [TOKEN]).unwrap();
        }));
        let (status, _) = get(
            fixture.state,
            &format!("/emby/Items?ParentId={}&Limit=2", fixture.a),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "populated={populate}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn latest_member_acl_change_during_second_segment_discards_earlier_item() {
    let mut fixture = Fixture::new(true);
    let app = rusqlite::Connection::open(fixture.app_db()).unwrap();
    app.execute(
        "UPDATE users SET role='member' WHERE id=(SELECT user_id FROM user_tokens WHERE token=?1)",
        [TOKEN],
    )
    .unwrap();
    let user: String = app
        .query_row(
            "SELECT user_id FROM user_tokens WHERE token=?1",
            [TOKEN],
            |row| row.get(0),
        )
        .unwrap();
    let db = fixture.library_db();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE libraries SET member_ids_json=?1 WHERE library_id=?2",
        rusqlite::params![serde_json::to_string(&[user]).unwrap(), &fixture.a],
    )
    .unwrap();
    let a = fixture.a.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    fixture.state = fixture
        .state
        .with_protocol_selection_barrier(Arc::new(move |_| {
            if observed.fetch_add(1, Ordering::SeqCst) == 1 {
                rusqlite::Connection::open(&db)
                    .unwrap()
                    .execute(
                        "UPDATE libraries SET member_ids_json='[]' WHERE library_id=?1",
                        [&a],
                    )
                    .unwrap();
            }
        }));
    let (status, body) = get(fixture.state, "/Users/me/Items/Latest?Limit=2", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), [fixture.b_item]);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn latest_metrics_publish_whole_final_response_and_one_complete_retry() {
    use media_server::query_metrics::QueryMetrics;
    let mut fixture = Fixture::new(true);
    let db = fixture.library_db();
    let a = fixture.a.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    fixture.state = fixture
        .state
        .with_protocol_selection_barrier(Arc::new(move |_| {
            if observed.fetch_add(1, Ordering::SeqCst) == 1 {
                hide(&rusqlite::Connection::open(&db).unwrap(), &a);
            }
        }));
    let timing = QueryTiming::start();
    let metrics = QueryMetrics::new(timing, true);
    let mut request = Request::get("/Users/me/Items/Latest?Limit=2")
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("X-Emby-Device-Id", "composition-device")
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(timing);
    request.extensions_mut().insert(metrics.clone());
    let response = router(fixture.state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(ids(&body), [fixture.b_item]);
    let s = metrics.snapshot();
    assert_eq!(
        s.returned,
        Some(1),
        "discarded segment rows cannot be published in metrics"
    );
    assert_eq!(s.total, Some(1), "Latest totals cover both final segments");
    assert_eq!(s.retry_count, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert!(s.vm_steps.unwrap() > 0 && s.statements.unwrap() > 0);
}

#[tokio::test]
async fn latest_metrics_leave_unselected_segment_total_absent_without_extra_query() {
    use media_server::query_metrics::QueryMetrics;
    let fixture = Fixture::new(true);
    let selected = Arc::new(AtomicUsize::new(0));
    let observed = selected.clone();
    let state = fixture
        .state
        .with_protocol_selection_barrier(Arc::new(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
        }));
    for (query, total, segments) in [
        ("Limit=1", None, 1),
        ("Limit=1&IsPlayed=false", Some(1), 1),
        ("Limit=2", Some(2), 2),
    ] {
        selected.store(0, Ordering::SeqCst);
        let timing = QueryTiming::start();
        let metrics = QueryMetrics::new(timing, true);
        let mut request = Request::get(format!("/Users/me/Items/Latest?{query}"))
            .header("Authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(timing);
        request.extensions_mut().insert(metrics.clone());
        let response = router(state.clone()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        let s = metrics.snapshot();
        assert_eq!(s.returned, Some(body.as_array().unwrap().len() as u64));
        assert_eq!(s.total, total);
        assert_eq!(s.retry_count, 0);
        assert_eq!(
            selected.load(Ordering::SeqCst),
            segments,
            "metrics cannot add a segment query"
        );
    }
}
