use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{User, UserId, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

struct RecordingNexusFetcher {
    urls: Mutex<Vec<String>>,
}

impl Fetcher for RecordingNexusFetcher {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        self.urls.lock().push(request.url.clone());
        Ok(include_str!("../../indexer/tests/fixtures/pterclub.html").into())
    }
}

struct TestHarness {
    _dir: tempfile::TempDir,
    app: axum::Router,
    store: Arc<Store>,
    fetcher: Arc<RecordingNexusFetcher>,
}

impl TestHarness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(RecordingNexusFetcher {
            urls: Mutex::new(Vec::new()),
        });
        let state = ApiState::new(
            Store::open(dir.path().join("data")).unwrap(),
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            fetcher.clone(),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        )
        .unwrap();
        let store = state.store();
        let app = router(state);
        Self {
            _dir: dir,
            app,
            store,
            fetcher,
        }
    }

    fn seed_user(&self, login: &str, role: UserRole, token: &str) {
        let user = User {
            id: UserId::new(),
            login: login.into(),
            enabled: true,
            role,
        };
        self.store
            .insert_user_with_password(&user, "password-123")
            .unwrap();
        self.store.set_user_token(user.id, token).unwrap();
    }
}

fn json_req(method: &str, uri: &str, token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn raw_req(method: &str, uri: &str, token: &str, raw_body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(raw_body.to_string()))
        .unwrap()
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn get_presets_for(app: &axum::Router, token: &str) -> Value {
    let res = app
        .clone()
        .oneshot(json_req(
            "GET",
            "/api/v1/search/presets",
            token,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    body_json(res).await["data"]["presets"].clone()
}

async fn put_presets_for(
    app: &axum::Router,
    token: &str,
    payload: Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(json_req("PUT", "/api/v1/search/presets", token, payload))
        .await
        .unwrap()
}

#[tokio::test]
async fn admin_presets_share_across_admins_and_inherit_for_members_until_overridden() {
    let h = TestHarness::new();
    h.seed_user("second-admin", UserRole::Admin, "admin2-token");
    h.seed_user("alice", UserRole::Member, "alice-token");
    h.seed_user("bob", UserRole::Member, "bob-token");

    let p1 = json!([{
        "type": "preset",
        "id": "p-uhd",
        "name": "4K 影剧",
        "visible": true,
        "categories": ["movie", "tv"],
        "site_ids": [],
        "poster_mode": false,
        "skip_history": false
    }]);
    let res = put_presets_for(&h.app, "management-secret", json!({ "presets": p1 })).await;
    assert_eq!(res.status(), StatusCode::OK);

    assert_eq!(get_presets_for(&h.app, "management-secret").await, p1);
    assert_eq!(get_presets_for(&h.app, "admin2-token").await, p1);
    assert_eq!(get_presets_for(&h.app, "alice-token").await, p1);
    assert_eq!(get_presets_for(&h.app, "bob-token").await, p1);

    // Second admin updates presets -> first admin and members without override see the update.
    let p2 = json!([{
        "type": "preset",
        "id": "p-anime",
        "name": "动漫专搜",
        "visible": true,
        "categories": ["anime"],
        "site_ids": [],
        "poster_mode": false,
        "skip_history": true
    }]);
    let res = put_presets_for(&h.app, "admin2-token", json!({ "presets": p2 })).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(get_presets_for(&h.app, "management-secret").await, p2);
    assert_eq!(get_presets_for(&h.app, "alice-token").await, p2);

    // Member Alice saves her own override -> isolated to Alice; Bob & Admins still see p2.
    let alice_custom = json!([{ "id": "p-alice", "name": "Alice Only" }]);
    let res = put_presets_for(&h.app, "alice-token", json!({ "presets": alice_custom })).await;
    assert_eq!(res.status(), StatusCode::OK);

    assert_eq!(get_presets_for(&h.app, "alice-token").await, alice_custom);
    assert_eq!(get_presets_for(&h.app, "bob-token").await, p2);
    assert_eq!(get_presets_for(&h.app, "management-secret").await, p2);
}

#[tokio::test]
async fn put_presets_rejects_invalid_payloads_with_400_and_deduplicates_ids() {
    let h = TestHarness::new();
    let initial = json!([{ "id": "p-keep", "name": "Keep Me" }]);
    let ok_res = put_presets_for(&h.app, "management-secret", json!({ "presets": initial })).await;
    assert_eq!(ok_res.status(), StatusCode::OK);

    for bad_body in [
        json!({}),
        json!({ "presets": null }),
        json!({ "presets": "not-an-array" }),
        json!({ "presets": 42 }),
        json!({ "presets": ["not-an-object"] }),
        json!({ "presets": [null] }),
        json!({ "presets": [{ "id": "", "name": "Empty ID" }] }),
        json!({ "presets": [{ "id": 123, "name": "Numeric ID" }] }),
    ] {
        let res = put_presets_for(&h.app, "management-secret", bad_body.clone()).await;
        assert_eq!(
            res.status(),
            StatusCode::BAD_REQUEST,
            "expected 400 for {bad_body}"
        );
        let err = body_json(res).await;
        assert_eq!(err["ok"], false);
        assert_eq!(err["error"]["code"], "search.invalid_presets");
    }

    let broken_json = h
        .app
        .clone()
        .oneshot(raw_req(
            "PUT",
            "/api/v1/search/presets",
            "management-secret",
            "{bad json",
        ))
        .await
        .unwrap();
    assert_eq!(broken_json.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(broken_json).await["error"]["code"],
        "search.invalid_presets"
    );

    // Existing saved presets were NOT wiped by invalid requests.
    assert_eq!(get_presets_for(&h.app, "management-secret").await, initial);

    // Duplicate IDs are deduplicated preserving first occurrence.
    let dup_res = put_presets_for(
        &h.app,
        "management-secret",
        json!({
            "presets": [
                { "id": "p-1", "name": "First" },
                { "id": "p-1", "name": "Duplicate" },
                { "id": "p-2", "name": "Second" }
            ]
        }),
    )
    .await;
    assert_eq!(dup_res.status(), StatusCode::OK);
    let expected = json!([
        { "id": "p-1", "name": "First" },
        { "id": "p-2", "name": "Second" }
    ]);
    assert_eq!(body_json(dup_res).await["data"]["presets"], expected);
    assert_eq!(get_presets_for(&h.app, "management-secret").await, expected);
}

#[tokio::test]
async fn nexusphp_category_search_and_deleted_site_scope_do_not_widen() {
    let h = TestHarness::new();

    let create_hdarea = h
        .app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/v1/sites",
            "management-secret",
            json!({
                "name": "HDArea",
                "url": "https://hdarea.example/",
                "profile_id": "hdarea",
                "cookie": "uid=1",
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(create_hdarea.status(), StatusCode::CREATED);
    let hdarea_id = body_json(create_hdarea).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let create_pter = h
        .app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/v1/sites",
            "management-secret",
            json!({
                "name": "PTer",
                "url": "https://pter.example/",
                "profile_id": "pterclub",
                "cookie": "uid=2",
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(create_pter.status(), StatusCode::CREATED);
    let pter_id = body_json(create_pter).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let pter_search = h
        .app
        .clone()
        .oneshot(json_req(
            "GET",
            &format!("/api/v1/search/torrents?keyword=friends&categories=movie,tv&sites={pter_id}"),
            "management-secret",
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(pter_search.status(), StatusCode::OK);
    assert_eq!(
        h.fetcher.urls.lock().as_slice(),
        &["https://pter.example/torrents.php?search=friends&cat401=1&cat402=1&cat403=1"]
    );

    h.fetcher.urls.lock().clear();
    let del = h
        .app
        .clone()
        .oneshot(json_req(
            "DELETE",
            &format!("/api/v1/sites/{pter_id}"),
            "management-secret",
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::OK);

    // Searching with a deleted ghost site ID must return 0 results and NOT widen to hdarea_id!
    let ghost_search = h
        .app
        .clone()
        .oneshot(json_req(
            "GET",
            &format!("/api/v1/search/torrents?keyword=friends&sites={pter_id}"),
            "management-secret",
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(ghost_search.status(), StatusCode::OK);
    let ghost_data = body_json(ghost_search).await["data"].clone();
    assert_eq!(ghost_data["total"], 0);
    assert!(ghost_data["items"].as_array().unwrap().is_empty());
    assert!(
        h.fetcher.urls.lock().is_empty(),
        "ghost site_id must not fall back to other enabled sites ({hdarea_id})"
    );
}
