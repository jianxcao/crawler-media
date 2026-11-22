use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use media::cache::{DISCOVER_TTL_SECS, is_discover_or_list_key, with_prefer_stale};
use media::response_cache::CacheSource;
use media::{CatalogGet, DEFAULT_TTL_SECS, Douban, Tmdb, TmdbError};

const NOW: i64 = 1_700_000_000;

fn tmdb_list_json(id: i64, title: &str) -> String {
    serde_json::json!({
        "page": 1,
        "total_pages": 1,
        "total_results": 1,
        "results": [{
            "id": id,
            "title": title,
            "name": title,
            "release_date": "2024-01-01",
            "first_air_date": "2024-01-01",
            "poster_path": "/p.jpg",
            "backdrop_path": "/b.jpg",
            "vote_average": 8.5,
            "overview": "overview"
        }]
    })
    .to_string()
}

fn tmdb_movie_detail_json(id: i64, title: &str) -> String {
    serde_json::json!({
        "id": id,
        "title": title,
        "original_title": title,
        "release_date": "1999-03-31",
        "poster_path": "/matrix.jpg",
        "overview": "detail"
    })
    .to_string()
}

fn douban_collection_json(id: &str, title: &str) -> String {
    serde_json::json!({
        "subject_collection_items": [{
            "id": id,
            "title": title,
            "year": "2024",
            "rating": { "value": 8.8 },
            "pic": { "normal": "https://img1.doubanio.com/view/photo/s_ratio_poster/public/p1.jpg" }
        }]
    })
    .to_string()
}

#[derive(Clone)]
struct GatedCatalog {
    state: Arc<GatedState>,
}

struct GatedState {
    bodies: Mutex<HashMap<String, String>>,
    blocked: Mutex<bool>,
    ready: Condvar,
}

impl GatedCatalog {
    fn new(bodies: HashMap<String, String>) -> Self {
        Self {
            state: Arc::new(GatedState {
                bodies: Mutex::new(bodies),
                blocked: Mutex::new(false),
                ready: Condvar::new(),
            }),
        }
    }

    fn set_blocked(&self, blocked: bool) {
        *self.state.blocked.lock().unwrap() = blocked;
        self.state.ready.notify_all();
    }

    fn set_body(&self, key_prefix: &str, body: String) {
        self.state
            .bodies
            .lock()
            .unwrap()
            .insert(key_prefix.to_string(), body);
    }
}

impl CatalogGet for GatedCatalog {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        let guard = self.state.blocked.lock().unwrap();
        let (guard, _) = self
            .state
            .ready
            .wait_timeout_while(guard, Duration::from_secs(5), |blocked| *blocked)
            .unwrap();
        if *guard {
            return Err(TmdbError::Http("upstream remained blocked".into()));
        }
        drop(guard);

        let bodies = self.state.bodies.lock().unwrap();
        bodies
            .iter()
            .find_map(|(prefix, body)| path.starts_with(prefix).then(|| body.clone()))
            .ok_or_else(|| TmdbError::Http(format!("unmapped path: {path}")))
    }
}

#[test]
fn discover_and_list_keys_use_24h_ttl_while_details_keep_7d_ttl() {
    assert_eq!(DISCOVER_TTL_SECS, 24 * 60 * 60);
    assert_eq!(DEFAULT_TTL_SECS, 7 * 24 * 60 * 60);

    for discover_key in [
        "/trending/movie/day?language=zh-CN",
        "/discover/movie?with_genres=28&language=zh-CN",
        "/movie/now_playing?language=zh-CN",
        "/movie/popular?language=zh-CN",
        "/tv/top_rated?language=zh-CN",
        "/movie/upcoming?language=zh-CN",
        "/tv/airing_today?language=zh-CN",
        "/tv/on_the_air?language=zh-CN",
        "/j/search_subjects?type=movie&tag=%E7%83%AD%E9%97%A8&page_limit=20&page_start=0",
        "/rexxar/api/v2/subject_collection/movie_hot_gaia/items?start=0&count=20",
        "/rexxar/api/v2/subject_collection/movie_showing/items?start=0&count=20",
        "/rexxar/api/v2/subject_collection/tv_hot/items?start=0&count=20",
    ] {
        assert!(
            is_discover_or_list_key(discover_key),
            "expected discover/list key: {discover_key}"
        );
    }
    for detail_key in [
        "/movie/603?language=zh-CN",
        "/tv/1396/season/1?language=zh-CN",
        "/person/287?append_to_response=combined_credits&language=zh-CN",
        "/subject/1292720/",
    ] {
        assert!(
            !is_discover_or_list_key(detail_key),
            "detail key must keep 7d TTL: {detail_key}"
        );
    }

    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let db = dir.path().join("cache.db");
    let http = GatedCatalog::new(HashMap::from([
        ("/movie/popular".into(), tmdb_list_json(101, "Popular V1")),
        (
            "/movie/603".into(),
            tmdb_movie_detail_json(603, "The Matrix V1"),
        ),
    ]));
    let tmdb = Tmdb::new_at(http.clone(), &db, NOW).unwrap();
    assert_eq!(tmdb.popular_movie().unwrap()[0].media.title, "Popular V1");
    assert_eq!(tmdb.movie_details("603").unwrap().title, "The Matrix V1");

    let conn = rusqlite::Connection::open(&db).unwrap();
    let rows = media::response_cache::list(&conn, Some(CacheSource::Tmdb)).unwrap();
    let pop_row = rows
        .iter()
        .find(|r| r.cache_key.starts_with("/movie/popular"))
        .unwrap();
    let det_row = rows
        .iter()
        .find(|r| r.cache_key.starts_with("/movie/603"))
        .unwrap();
    assert_eq!(pop_row.expires_at, Some(NOW + DISCOVER_TTL_SECS));
    assert_eq!(det_row.expires_at, Some(NOW + DEFAULT_TTL_SECS));

    // After 24h + 1s: discover key refreshes to V2, while detail key stays fresh on V1.
    http.set_body("/movie/popular", tmdb_list_json(101, "Popular V2"));
    http.set_body("/movie/603", tmdb_movie_detail_json(603, "The Matrix V2"));
    tmdb.set_now(NOW + DISCOVER_TTL_SECS + 1);
    assert_eq!(tmdb.popular_movie().unwrap()[0].media.title, "Popular V2");
    assert_eq!(tmdb.movie_details("603").unwrap().title, "The Matrix V1");
}

#[test]
fn prefer_stale_serves_expired_discover_entries_instantly_without_blocking_on_upstream() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let db = dir.path().join("cache.db");
    let http = GatedCatalog::new(HashMap::from([
        ("/movie/popular".into(), tmdb_list_json(1, "TMDB V1")),
        (
            "/rexxar/api/v2/subject_collection/movie_hot_gaia".into(),
            douban_collection_json("1292720", "Douban V1"),
        ),
        (
            "/movie/top_rated".into(),
            tmdb_list_json(2, "Cold Top Rated"),
        ),
        (
            "/movie/603".into(),
            tmdb_movie_detail_json(603, "Matrix V1"),
        ),
    ]));
    let tmdb = Tmdb::new_at(http.clone(), &db, NOW).unwrap();
    let douban = Douban::new_at(http.clone(), &db, NOW).unwrap();

    assert_eq!(tmdb.popular_movie().unwrap()[0].media.title, "TMDB V1");
    assert_eq!(douban.trending_movie().unwrap()[0].media.title, "Douban V1");
    assert_eq!(tmdb.movie_details("603").unwrap().title, "Matrix V1");

    // Expire the 24h discover entries and block upstream completely.
    tmdb.set_now(NOW + DISCOVER_TTL_SECS + 10);
    douban.set_now(NOW + DISCOVER_TTL_SECS + 10);
    http.set_blocked(true);

    let start = Instant::now();
    let tmdb_swr = with_prefer_stale(|| tmdb.popular_movie());
    let douban_swr = with_prefer_stale(|| douban.trending_movie());
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "SWR read must not wait on blocked upstream: {:?}",
        start.elapsed()
    );
    assert!(tmdb_swr.used_stale());
    assert_eq!(tmdb_swr.value.unwrap()[0].media.title, "TMDB V1");
    assert!(douban_swr.used_stale());
    assert_eq!(douban_swr.value.unwrap()[0].media.title, "Douban V1");

    // Unblock upstream with V2 payloads: background/origin refresh updates cache.db,
    // and cold discover keys still fetch synchronously on first read.
    http.set_body("/movie/popular", tmdb_list_json(1, "TMDB V2"));
    http.set_body(
        "/rexxar/api/v2/subject_collection/movie_hot_gaia",
        douban_collection_json("1292720", "Douban V2"),
    );
    http.set_blocked(false);

    let cold = with_prefer_stale(|| tmdb.top_rated_movie());
    assert!(!cold.used_stale());
    assert_eq!(cold.value.unwrap()[0].media.title, "Cold Top Rated");

    // Simulate background revalidation in normal Origin mode:
    assert_eq!(tmdb.popular_movie().unwrap()[0].media.title, "TMDB V2");
    assert_eq!(douban.trending_movie().unwrap()[0].media.title, "Douban V2");

    // Subsequent PreferStale read hits the refreshed 24h entry as fresh.
    let after_refresh = with_prefer_stale(|| tmdb.popular_movie());
    assert!(!after_refresh.used_stale());
    assert_eq!(after_refresh.value.unwrap()[0].media.title, "TMDB V2");

    // Detail keys expired past 7d do not use discover SWR bypass.
    http.set_body("/movie/603", tmdb_movie_detail_json(603, "Matrix V2"));
    tmdb.set_now(NOW + DEFAULT_TTL_SECS + 10);
    let detail_read = with_prefer_stale(|| tmdb.movie_details("603"));
    assert!(!detail_read.used_stale());
    assert_eq!(detail_read.value.unwrap().title, "Matrix V2");
}
