//! today-arrivals uses adopted or cached facts only. Cache reads and rendering
//! run outside the Store read lease and never hold the Library writer.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex as StdMutex};

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{
    Coverage, FetchMode, Media, MediaId, MediaKind, Subscribe, SubscribeId, User, UserId, UserRole,
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media::{CatalogGet, Tmdb, TmdbError};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "management-secret";
const MEMBER_TOKEN: &str = "member-token";
const ARRIVALS: &str = "/api/v1/subscriptions/today-arrivals";

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

fn make_state(root: &Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

fn today() -> String {
    store::rfc3339_from_secs(store::unix_now())[..10].to_string()
}

fn season_payload(date: &str) -> String {
    json!({
        "id": 1,
        "season_number": 1,
        "name": "Season 1",
        "episodes": [{ "episode_number": 1, "air_date": date }],
    })
    .to_string()
}

/// 永不走网络；只有缓存命中才可能成功。
struct NeverGet;

impl CatalogGet for NeverGet {
    fn get(&self, _path: &str) -> Result<String, TmdbError> {
        panic!("arrivals must use cached facts without requesting origin")
    }
}

/// 只用于把响应写进 Tmdb 自己的响应缓存，之后用 `NeverGet` 验证离线命中。
struct StaticGet {
    body: String,
}

impl CatalogGet for StaticGet {
    fn get(&self, _path: &str) -> Result<String, TmdbError> {
        Ok(self.body.clone())
    }
}

/// 用 Tmdb 客户端的公开路径写入它自己的响应缓存。
///
/// `now` 为 0 时使用实时时钟（新鲜）；非 0 时钉住写入时刻，用来制造已过期条目。
fn seed_tmdb_cache(path: &Path, body: &str, now: i64) {
    seed_tmdb_cache_for(path, body, now, "100");
}

fn seed_tmdb_cache_for(path: &Path, body: &str, now: i64, tmdb_id: &str) {
    let get = StaticGet {
        body: body.to_string(),
    };
    let tmdb = if now == 0 {
        Tmdb::new(get, path).unwrap()
    } else {
        Tmdb::new_at(get, path, now).unwrap()
    };
    tmdb.season_episodes(tmdb_id, 1)
        .expect("seeding the Tmdb cache must succeed");
}

/// Local cached facts may need another Store reader. The gate models a finite
/// cache DB read, never an optional origin request.
struct CachedFacts {
    store: Arc<Store>,
    gate: Option<Arc<Gate>>,
    calls: Arc<AtomicUsize>,
}
impl api::catalog::Catalog for CachedFacts {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        panic!("arrivals must not request catalog origin")
    }
    fn cached_season_episodes(
        &self,
        _: &str,
        _: u32,
    ) -> Result<Option<Vec<media::SeasonEpisode>>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.enter_and_wait();
        }
        self.store
            .get_setting("tmdb.api_key")
            .map_err(|error| error.to_string())?;
        Ok(Some(vec![media::SeasonEpisode {
            episode_number: 1,
            air_date: Some(today()),
        }]))
    }
}

struct Gate {
    entered: StdMutex<bool>,
    entered_cv: Condvar,
    open: StdMutex<bool>,
    open_cv: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            entered: StdMutex::new(false),
            entered_cv: Condvar::new(),
            open: StdMutex::new(false),
            open_cv: Condvar::new(),
        }
    }

    fn enter_and_wait(&self) {
        *self.entered.lock().unwrap() = true;
        self.entered_cv.notify_all();
        let released = *self
            .open_cv
            .wait_timeout_while(
                self.open.lock().unwrap(),
                std::time::Duration::from_secs(30),
                |open| !*open,
            )
            .unwrap()
            .0;
        assert!(released, "test must release the arrivals gate");
    }

    fn wait_entered(&self) {
        let entered = *self
            .entered_cv
            .wait_timeout_while(
                self.entered.lock().unwrap(),
                std::time::Duration::from_secs(10),
                |entered| !*entered,
            )
            .unwrap()
            .0;
        assert!(entered, "worker must enter the arrivals gate");
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.open_cv.notify_all();
    }
}

/// 断言失败时也要放行阻塞 closure，避免 runtime 关闭时卡住。
struct ReleaseOnDrop(Arc<Gate>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn insert_tv_media(store: &Store, title: &str, tmdb_id: &str) -> MediaId {
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: title.into(),
            year: Some(2026),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some(tmdb_id.into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    media_id
}

fn insert_tv_subscribe_for_media(store: &Store, user_id: UserId, media_id: MediaId) -> SubscribeId {
    let filter_id = store.list_filters().unwrap()[0].id;
    let subscribe_id = SubscribeId::new();
    store
        .insert_subscribe(&Subscribe {
            id: subscribe_id,
            user_id,
            media_id,
            coverage: Coverage::Tv {
                season: 1,
                episode_from: 1,
                episode_to: Some(2),
            },
            fetch_mode: FetchMode::Search,
            filter_id,
            wash_cut: false,
            wash_cut_filter_id: None,
            keep_old_versions: false,
            full_season_pack: false,
            downloader_id: None,
            library_id: None,
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
        })
        .unwrap();
    subscribe_id
}

fn insert_tv_subscribe(store: &Store, user_id: UserId, title: &str, tmdb_id: &str) -> SubscribeId {
    let media_id = insert_tv_media(store, title, tmdb_id);
    insert_tv_subscribe_for_media(store, user_id, media_id)
}

fn seed_admin_tv(state: &ApiState, title: &str, tmdb_id: &str) -> SubscribeId {
    seed_admin_tv_with_media(state, title, tmdb_id).0
}

fn seed_admin_tv_with_media(
    state: &ApiState,
    title: &str,
    tmdb_id: &str,
) -> (SubscribeId, MediaId) {
    let store = state.store();
    let guard = store.as_ref();
    let user_id = guard.user_id_by_token(TOKEN).unwrap().unwrap();
    guard
        .put_setting("tmdb.api_key", "offline-never-sent")
        .unwrap();
    let media_id = insert_tv_media(guard, title, tmdb_id);
    let subscribe_id = insert_tv_subscribe_for_media(guard, user_id, media_id);
    (subscribe_id, media_id)
}

fn seed_member_tv(state: &ApiState, title: &str, tmdb_id: &str) -> SubscribeId {
    let store = state.store();
    let guard = store.as_ref();
    let user_id = UserId::new();
    guard
        .insert_user_with_password(
            &User {
                id: user_id,
                login: "member".into(),
                enabled: true,
                role: UserRole::Member,
            },
            "member-secret",
        )
        .unwrap();
    guard.set_user_token(user_id, MEMBER_TOKEN).unwrap();
    insert_tv_subscribe(guard, user_id, title, tmdb_id)
}

fn arrivals_request(token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(ARRIVALS)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

async fn json_data(response: axum::response::Response) -> Value {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["ok"], true, "status={status} body={body}");
    body.get("data").cloned().unwrap_or(Value::Null)
}

fn has_wanted(rows: &Value, subscription_id: SubscribeId) -> bool {
    rows.as_array().is_some_and(|rows| {
        rows.iter().any(|row| {
            row["status"] == "wanted" && row["subscription_id"] == subscription_id.to_string()
        })
    })
}

#[tokio::test]
async fn arrivals_cold_catalog_does_not_predict_unknown_dates() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let subscribe = seed_admin_tv(&state, "Cold Show", "100");
    let tmdb = Tmdb::new(NeverGet, &tmp.path().join("data/catalog.db")).unwrap();
    let app = router(state.with_catalog(api::catalog::TmdbCatalog::new(tmdb)));

    let response = app.oneshot(arrivals_request(TOKEN)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rows = json_data(response).await;
    assert!(
        !has_wanted(&rows, subscribe) && rows.as_array().unwrap().is_empty(),
        "cold-cache arrivals must not fabricate air dates: {rows}"
    );
}

#[tokio::test]
async fn arrivals_fresh_cache_returns_forecast_without_network() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let subscribe = seed_admin_tv(&state, "Warm Show", "100");
    seed_tmdb_cache(
        &tmp.path().join("data/catalog.db"),
        &season_payload(&today()),
        0,
    );
    let tmdb = Tmdb::new(NeverGet, &tmp.path().join("data/catalog.db")).unwrap();
    let app = router(state.with_catalog(api::catalog::TmdbCatalog::new(tmdb)));

    let response = app.oneshot(arrivals_request(TOKEN)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rows = json_data(response).await;
    assert!(
        has_wanted(&rows, subscribe),
        "fresh cache must serve the forecast offline: {rows}"
    );
}

#[tokio::test]
async fn arrivals_expired_cache_falls_back_to_stale_on_network_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let subscribe = seed_admin_tv(&state, "Stale Show", "100");
    seed_tmdb_cache(
        &tmp.path().join("data/catalog.db"),
        &season_payload(&today()),
        store::unix_now() - 8 * 86_400,
    );
    let tmdb = Tmdb::new(NeverGet, &tmp.path().join("data/catalog.db")).unwrap();
    let app = router(state.with_catalog(api::catalog::TmdbCatalog::new(tmdb)));

    let response = app.oneshot(arrivals_request(TOKEN)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rows = json_data(response).await;
    assert!(
        has_wanted(&rows, subscribe),
        "expired cache plus network failure must fall back to stale: {rows}"
    );
}

#[tokio::test]
async fn arrivals_isolate_users() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let admin_subscribe = seed_admin_tv(&state, "Admin Show", "100");
    let member_subscribe = seed_member_tv(&state, "Member Show", "200");
    let cache = tmp.path().join("data/catalog.db");
    for tmdb_id in ["100", "200"] {
        seed_tmdb_cache_for(&cache, &season_payload(&today()), 0, tmdb_id);
    }
    let tmdb = Tmdb::new(NeverGet, &cache).unwrap();
    let app = router(state.with_catalog(api::catalog::TmdbCatalog::new(tmdb)));

    let admin_rows = json_data(app.clone().oneshot(arrivals_request(TOKEN)).await.unwrap()).await;
    assert!(has_wanted(&admin_rows, admin_subscribe));
    assert!(
        has_wanted(&admin_rows, member_subscribe),
        "admin sees every subscribe: {admin_rows}"
    );

    let member_rows = json_data(app.oneshot(arrivals_request(MEMBER_TOKEN)).await.unwrap()).await;
    let member_rows = member_rows.as_array().unwrap();
    assert_eq!(
        member_rows.len(),
        1,
        "member must only see own rows: {member_rows:?}"
    );
    assert_eq!(
        member_rows[0]["subscription_id"],
        member_subscribe.to_string()
    );
}

#[tokio::test]
async fn arrivals_missing_cached_source_keeps_other_items() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let ok_subscribe = seed_admin_tv(&state, "Reachable Show", "100");
    let failing_subscribe = seed_admin_tv(&state, "Unreachable Show", "200");
    let cache = tmp.path().join("data/catalog.db");
    seed_tmdb_cache(&cache, &season_payload(&today()), 0);
    let tmdb = Tmdb::new(NeverGet, &cache).unwrap();
    let app = router(state.with_catalog(api::catalog::TmdbCatalog::new(tmdb)));

    let response = app.oneshot(arrivals_request(TOKEN)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rows = json_data(response).await;
    assert!(
        has_wanted(&rows, ok_subscribe),
        "reachable source must keep its forecast: {rows}"
    );
    assert!(
        !has_wanted(&rows, failing_subscribe),
        "failing source must not fabricate a forecast: {rows}"
    );
}

#[tokio::test]
async fn arrivals_query_each_season_once_per_request() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let (first, media_id) = seed_admin_tv_with_media(&state, "Show A", "100");
    let second = {
        let store = state.store();
        let guard = store.as_ref();
        let user_id = guard.user_id_by_token(TOKEN).unwrap().unwrap();
        insert_tv_subscribe_for_media(guard, user_id, media_id)
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let catalog = CachedFacts {
        store: state.store(),
        gate: None,
        calls: calls.clone(),
    };
    let app = router(state.with_catalog(Arc::new(catalog)));

    let rows = json_data(app.oneshot(arrivals_request(TOKEN)).await.unwrap()).await;
    assert!(
        has_wanted(&rows, first) && has_wanted(&rows, second),
        "{rows}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the same (tmdb_id, season) must be queried once per request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_cached_read_does_not_hold_store_writer() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    seed_admin_tv(&state, "Blocked Show", "100");
    let gate = Arc::new(Gate::new());
    let catalog = CachedFacts {
        store: state.store(),
        gate: Some(gate.clone()),
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let app = router(state.clone().with_catalog(Arc::new(catalog)));

    let handle = tokio::spawn({
        let app = app.clone();
        async move { app.oneshot(arrivals_request(TOKEN)).await.unwrap() }
    });
    gate.wait_entered();
    let _release = ReleaseOnDrop(gate.clone());
    {
        let store = state.store();
        assert!(
            store
                .writers()
                .with_writer_timeout(std::time::Duration::ZERO, store::Db::Library, |_| Ok(()),)
                .is_ok(),
            "arrivals must not hold the Library writer while the catalog call blocks"
        );
    }
    gate.release();
    let response = handle.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rows = json_data(response).await;
    assert!(
        rows.as_array().is_some_and(|rows| !rows.is_empty()),
        "{rows}"
    );
}
