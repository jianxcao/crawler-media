//! Integration coverage for Discover single-source isolation, 24h SWR caching,
//! viewer-scoped local library ownership (`in_library` / `library_status`), and
//! the daily `discover_refresh` prewarm job.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use api::catalog::{DoubanCatalog, FanoutCatalog, TmdbCatalog};
use api::worker::StateRunner;
use api::{ApiState, Store, router};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind, User, UserId, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use jobs::{JobIdentity, JobKind, Runner};
use media::response_cache::CacheSource;
use media::{CatalogGet, Douban, Tmdb, TmdbError};
use serde_json::Value;
use tower::ServiceExt;

const ADMIN_TOKEN: &str = "discover-swr-admin-token";
const MEMBER_TOKEN: &str = "discover-swr-member-token";

struct OfflineFetcher;

impl Fetcher for OfflineFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline in discover test".into()))
    }
}

#[derive(Clone)]
struct FakeHttp {
    inner: Arc<FakeHttpState>,
}

struct FakeHttpState {
    label: Mutex<String>,
    calls: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    blocked: Mutex<bool>,
    ready: Condvar,
    fail_prefix: Mutex<Option<String>>,
    is_douban: bool,
}

impl FakeHttp {
    fn tmdb(label: &str) -> Self {
        Self::new(label, false)
    }

    fn douban(label: &str) -> Self {
        Self::new(label, true)
    }

    fn new(label: &str, is_douban: bool) -> Self {
        Self {
            inner: Arc::new(FakeHttpState {
                label: Mutex::new(label.into()),
                calls: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                blocked: Mutex::new(false),
                ready: Condvar::new(),
                fail_prefix: Mutex::new(None),
                is_douban,
            }),
        }
    }

    fn calls(&self) -> usize {
        self.inner.calls.load(Ordering::SeqCst)
    }

    fn peak(&self) -> usize {
        self.inner.peak.load(Ordering::SeqCst)
    }

    fn set_label(&self, label: &str) {
        *self.inner.label.lock().unwrap() = label.into();
    }

    fn set_blocked(&self, blocked: bool) {
        *self.inner.blocked.lock().unwrap() = blocked;
        self.inner.ready.notify_all();
    }

    fn set_fail_prefix(&self, prefix: Option<&str>) {
        *self.inner.fail_prefix.lock().unwrap() = prefix.map(str::to_string);
    }
}

impl CatalogGet for FakeHttp {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        self.inner.calls.fetch_add(1, Ordering::SeqCst);
        let cur = self.inner.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.inner.peak.fetch_max(cur, Ordering::SeqCst);

        let guard = self.inner.blocked.lock().unwrap();
        let (guard, _) = self
            .inner
            .ready
            .wait_timeout_while(guard, Duration::from_secs(5), |b| *b)
            .unwrap();
        let still_blocked = *guard;
        drop(guard);

        let fail_match = self
            .inner
            .fail_prefix
            .lock()
            .unwrap()
            .as_deref()
            .is_some_and(|prefix| path.starts_with(prefix));
        let label = self.inner.label.lock().unwrap().clone();
        std::thread::sleep(Duration::from_millis(5));
        self.inner.active.fetch_sub(1, Ordering::SeqCst);

        if still_blocked {
            return Err(TmdbError::Http("blocked in test".into()));
        }
        if fail_match {
            return Err(TmdbError::Http(format!(
                "simulated upstream error on {path}"
            )));
        }

        if self.inner.is_douban {
            if path.contains("subject_collection") {
                Ok(serde_json::json!({
                    "subject_collection_items": [{
                        "id": "1291843",
                        "title": format!("Douban {label}"),
                        "year": "1999",
                        "rating": { "value": 9.1 },
                        "pic": { "normal": "https://img1.doubanio.com/view/photo/s_ratio_poster/public/p1.jpg" }
                    }]
                })
                .to_string())
            } else {
                Ok(serde_json::json!({
                    "subjects": [{
                        "id": "1291843",
                        "title": format!("Douban {label}"),
                        "cover": "https://img1.doubanio.com/view/photo/s_ratio_poster/public/p1.jpg",
                        "rate": "9.1"
                    }]
                })
                .to_string())
            }
        } else {
            Ok(serde_json::json!({
                "page": 1,
                "total_pages": 1,
                "total_results": 2,
                "results": [
                    {
                        "id": 603,
                        "title": format!("Matrix {label}"),
                        "name": format!("Matrix {label}"),
                        "release_date": "1999-03-31",
                        "first_air_date": "1999-03-31",
                        "poster_path": "/matrix.jpg",
                        "backdrop_path": "/matrix-bg.jpg",
                        "vote_average": 8.7,
                        "overview": "Neo"
                    },
                    {
                        "id": 604,
                        "title": format!("Reloaded {label}"),
                        "name": format!("Reloaded {label}"),
                        "release_date": "2003-05-15",
                        "first_air_date": "2003-05-15",
                        "poster_path": "/reloaded.jpg",
                        "backdrop_path": "/reloaded-bg.jpg",
                        "vote_average": 7.2,
                        "overview": "Zion"
                    }
                ]
            })
            .to_string())
        }
    }
}

fn build_fixture() -> (tempfile::TempDir, ApiState, FakeHttp, FakeHttp) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let state = ApiState::new(
        store,
        ADMIN_TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(OfflineFetcher),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
    )
    .unwrap();
    let tmdb_http = FakeHttp::tmdb("V1");
    let douban_http = FakeHttp::douban("V1");
    let tmdb = Tmdb::with_cache(tmdb_http.clone(), state.catalog_cache(CacheSource::Tmdb));
    let douban = Douban::with_cache(
        douban_http.clone(),
        state.catalog_cache(CacheSource::Douban),
    );
    let fanout = FanoutCatalog::new(vec![TmdbCatalog::new(tmdb), DoubanCatalog::new(douban)]);
    let state = state.with_catalog(fanout);
    (dir, state, tmdb_http, douban_http)
}

async fn get_json(app: Router, uri: &str, token: &str) -> Value {
    let req = Request::get(uri)
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "GET {uri}");
    serde_json::from_slice(&to_bytes(resp.into_body(), 2 * 1024 * 1024).await.unwrap()).unwrap()
}

fn seed_admin_movie_and_member(dir: &std::path::Path, store: &Store) -> MediaId {
    let media_id = MediaId::new();
    let movies_dir = dir.join("movies");
    std::fs::create_dir_all(&movies_dir).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Admin Movies",
            &[movies_dir.to_str().unwrap()],
            "selected",
            true,
            &[],
        )
        .unwrap();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: "The Matrix".into(),
            year: Some(1999),
            original_title: Some("The Matrix".into()),
            aliases: Vec::new(),
            tmdb_id: Some("603".into()),
            douban_id: Some("1291843".into()),
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let file_path = movies_dir.join("The.Matrix.1999.2160p.mkv");
    std::fs::write(&file_path, b"matrix").unwrap();
    store::test_support::publish_fixture_file(
        store,
        &store::test_support::FixtureFile {
            media_id,
            path: file_path.display().to_string(),
            resolution: Some("2160p".into()),
            ..Default::default()
        },
    )
    .unwrap();

    let member = User {
        id: UserId::new(),
        login: "viewer-member".into(),
        enabled: true,
        role: UserRole::Member,
    };
    store.insert_user(&member).unwrap();
    store.set_user_token(member.id, MEMBER_TOKEN).unwrap();
    media_id
}

#[test]
fn discover_source_tmdb_isolates_tmdb_and_enriches_viewer_scoped_ownership() {
    let (dir, state, _tmdb_http, douban_http) = build_fixture();
    let media_id = seed_admin_movie_and_member(dir.path(), &state.store());

    let app = router(state);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let wall = get_json(
            app.clone(),
            "/api/v1/discover/movie?source=tmdb",
            ADMIN_TOKEN,
        )
        .await;
        let filtered = get_json(
            app.clone(),
            "/api/v1/discover/movie/filtered?source=tmdb&sort=rating",
            ADMIN_TOKEN,
        )
        .await;
        let collection = get_json(
            app.clone(),
            "/api/v1/discover/movie/collection/popular?source=tmdb&page=1&page_size=20",
            ADMIN_TOKEN,
        )
        .await;
        assert_eq!(
            douban_http.calls(),
            0,
            "source=tmdb must never fan out to Douban"
        );

        for items in [
            wall["data"]["sections"][0]["items"].as_array().unwrap(),
            filtered["data"]["items"].as_array().unwrap(),
            collection["data"]["items"].as_array().unwrap(),
        ] {
            assert_eq!(items[0]["id"], "603");
            assert_eq!(items[0]["in_library"], true);
            assert_eq!(items[0]["library_status"]["owned"], true);
            assert_eq!(
                items[0]["library_status"]["media_item_id"],
                media_id.to_string()
            );
            assert_eq!(items[0]["library_status"]["library_count"], 1);
            assert_eq!(items[0]["library_status"]["file_count"], 1);
            assert_eq!(items[1]["id"], "604");
            assert_eq!(items[1]["in_library"], false);
            assert!(items[1].get("library_status").is_none());
        }

        // Restricted library must not leak ownership to an unauthorized member.
        let member_wall = get_json(
            app.clone(),
            "/api/v1/discover/movie?source=tmdb",
            MEMBER_TOKEN,
        )
        .await;
        let member_item = &member_wall["data"]["sections"][0]["items"][0];
        assert_eq!(member_item["in_library"], false);
        assert!(member_item.get("library_status").is_none());

        // Douban wall also resolves local ownership via douban_id alias.
        let douban_wall = get_json(app, "/api/v1/discover/movie?source=douban", ADMIN_TOKEN).await;
        let douban_item = &douban_wall["data"]["sections"][0]["items"][0];
        assert_eq!(douban_item["id"], "1291843");
        assert_eq!(douban_item["in_library"], true);
        assert_eq!(douban_item["library_status"]["owned"], true);
    });
}

#[test]
fn expired_discover_wall_serves_stale_immediately_and_revalidates_in_background() {
    let (_dir, state, tmdb_http, _douban_http) = build_fixture();
    let app = router(state.clone());

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let initial =
            get_json(app.clone(), "/api/v1/discover/movie?source=tmdb", ADMIN_TOKEN).await;
        assert_eq!(
            initial["data"]["sections"][0]["items"][0]["title"],
            "Matrix V1"
        );

        // Expire every cached TMDB entry so get_fresh misses across all 20 sections.
        for row in state.store().list_catalog_cache().unwrap() {
            state
                .store()
                .put_catalog_cache(&row.source, &row.cache_key, &row.body, row.fetched_at, Some(1))
                .unwrap();
        }

        tmdb_http.set_label("V2");
        tmdb_http.set_blocked(true);

        let start = Instant::now();
        let stale_resp =
            get_json(app.clone(), "/api/v1/discover/movie?source=tmdb", ADMIN_TOKEN).await;
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "expired Discover wall must return stale data immediately without waiting on upstream: {:?}",
            start.elapsed()
        );
        assert_eq!(
            stale_resp["data"]["sections"][0]["items"][0]["title"],
            "Matrix V1"
        );

        // Release upstream so the spawned background SWR task finishes refreshing cache.db.
        tmdb_http.set_blocked(false);
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut refreshed = false;
        while Instant::now() < deadline {
            let next =
                get_json(app.clone(), "/api/v1/discover/movie?source=tmdb", ADMIN_TOKEN).await;
            if next["data"]["sections"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["items"][0]["title"] == "Matrix V2")
            {
                refreshed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert!(
            refreshed,
            "background SWR refresh must update all stale sections to Matrix V2"
        );
    });
}

#[test]
fn discover_refresh_job_is_seeded_daily_and_prewarms_tmdb_and_douban_gently() {
    let (_dir, state, tmdb_http, douban_http) = build_fixture();
    let app = router(state.clone());

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let jobs_body = get_json(app, "/api/v1/jobs?scope=system", ADMIN_TOKEN).await;
        let defs = jobs_body["data"].as_array().unwrap();
        let discover_def = defs
            .iter()
            .find(|row| row["kind"] == "discover_refresh")
            .expect("discover_refresh system job must be seeded on startup");
        assert_eq!(discover_def["friendly_name"], "发现页榜单每日预热");
        assert_eq!(discover_def["enabled"], true);
        assert_eq!(discover_def["schedule"]["interval_secs"], 86_400);
        assert_eq!(discover_def["concurrency_key"], "discover_refresh");
    });

    // Without TMDB key configured, job refreshes Douban only and tolerates partial failure.
    douban_http.set_fail_prefix(Some("/rexxar/api/v2/subject_collection/tv_hot"));
    let job = {
        let queue = state.jobs();
        let guard = queue.acquire().unwrap();
        let enqueued = guard
            .enqueue(JobIdentity::global(JobKind::DiscoverRefresh), "{}", 0)
            .unwrap();
        guard
            .claim(0, 10)
            .unwrap()
            .into_iter()
            .find(|j| j.id == enqueued.id || j.kind == JobKind::DiscoverRefresh)
            .unwrap()
    };
    StateRunner {
        state: state.clone(),
    }
    .run(&job)
    .expect("discover_refresh must succeed best-effort when TMDB is unconfigured");
    assert_eq!(
        tmdb_http.calls(),
        0,
        "unconfigured TMDB must be skipped during prewarm"
    );
    assert!(douban_http.calls() > 0);
    assert!(douban_http.peak() <= 2);

    // Configure TMDB API key: prewarm populates both TMDB and Douban with 24h TTL and <=2 concurrency.
    douban_http.set_fail_prefix(None);
    state
        .store()
        .put_setting(api::settings_keys::TMDB_API_KEY, "test-tmdb-key")
        .unwrap();
    StateRunner {
        state: state.clone(),
    }
    .run(&job)
    .expect("discover_refresh must prewarm TMDB and Douban sections");

    assert!(tmdb_http.calls() > 0);
    assert!(
        tmdb_http.peak() <= 2,
        "discover_refresh concurrency must stay <= 2, got {}",
        tmdb_http.peak()
    );
    let cached = state.store().list_catalog_cache().unwrap();
    assert!(cached.iter().any(|r| r.source == "tmdb"));
    assert!(cached.iter().any(|r| r.source == "douban"));
    for row in &cached {
        assert_eq!(
            row.expires_at.map(|exp| exp - row.fetched_at),
            Some(media::cache::DISCOVER_TTL_SECS),
            "prewarmed discover row {} must carry 24h TTL",
            row.cache_key
        );
    }
}
