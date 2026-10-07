use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use domain::MediaKind;
use media::{CatalogGet, Douban, Tmdb, TmdbError};

struct FakeHttp {
    bodies: HashMap<String, String>,
    hits: Arc<Mutex<Vec<String>>>,
}

impl CatalogGet for FakeHttp {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        self.hits.lock().unwrap().push(path.to_string());
        // Douban paths carry no language; TMDB usage in the shared-cache test
        // appends language=zh-CN. Drop it so one fake serves both.
        let key = path
            .replace("&language=zh-CN", "")
            .replace("?language=zh-CN", "");
        self.bodies
            .get(&key)
            .cloned()
            .ok_or_else(|| TmdbError::Http(format!("missing {key}")))
    }
}

fn movie_path() -> String {
    "/j/subject_suggest?q=matrix".into()
}

fn tv_path() -> String {
    "/j/subject_suggest?q=breaking".into()
}

fn popular_path() -> String {
    "/j/search_subjects?type=movie&tag=%E7%83%AD%E9%97%A8&page_limit=20&page_start=0".into()
}

fn trending_tv_path() -> String {
    "/rexxar/api/v2/subject_collection/tv_hot/items?start=0&count=20".into()
}

fn trending_movie_path() -> String {
    "/rexxar/api/v2/subject_collection/movie_hot_gaia/items?start=0&count=20".into()
}

fn now_playing_movie_path() -> String {
    "/rexxar/api/v2/subject_collection/movie_showing/items?start=0&count=20".into()
}

fn bodies() -> HashMap<String, String> {
    HashMap::from([
        (
            movie_path(),
            include_str!("fixtures/douban_search_movie.json").into(),
        ),
        (
            tv_path(),
            include_str!("fixtures/douban_search_tv.json").into(),
        ),
        (
            popular_path(),
            include_str!("fixtures/douban_popular_movie.json").into(),
        ),
        (
            trending_tv_path(),
            include_str!("fixtures/douban_subject_collection_tv.json").into(),
        ),
        (
            trending_movie_path(),
            include_str!("fixtures/douban_subject_collection_movie.json").into(),
        ),
        (
            now_playing_movie_path(),
            include_str!("fixtures/douban_movie_showing.json").into(),
        ),
    ])
}

fn client(dir: &std::path::Path) -> Douban<FakeHttp> {
    Douban::new(
        FakeHttp {
            bodies: bodies(),
            hits: Arc::new(Mutex::new(Vec::new())),
        },
        &dir.join("catalog.db"),
    )
    .unwrap()
}

#[test]
fn search_movie_returns_pickable_media() {
    let dir = tempfile::tempdir().unwrap();
    let hits = client(dir.path()).search_movie("matrix").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].media.douban_id.as_deref(), Some("1291843"));
    assert!(hits[0].media.tmdb_id.is_none());
    assert_eq!(hits[0].media.title, "黑客帝国");
    assert_eq!(hits[0].media.kind, MediaKind::Movie);
    assert_eq!(hits[0].media.year, Some(1999));
    assert_eq!(
        hits[0].poster_path.as_deref(),
        Some("https://img2.doubanio.com/view/photo/s_ratio_poster/public/p513344864.jpg")
    );
}

#[test]
fn search_tv_returns_pickable_media() {
    let dir = tempfile::tempdir().unwrap();
    let hits = client(dir.path()).search_tv("breaking").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].media.douban_id.as_deref(), Some("2156490"));
    assert_eq!(hits[0].media.kind, MediaKind::Tv);
    assert_eq!(hits[0].media.title, "绝命毒师");
}

#[test]
fn popular_movie_returns_pickable_media() {
    let dir = tempfile::tempdir().unwrap();
    let hits = client(dir.path()).popular_movie().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].media.douban_id.as_deref(), Some("1291843"));
    assert!(hits[0].media.tmdb_id.is_none());
}

#[test]
fn trending_tv_returns_pickable_media_from_collection() {
    let dir = tempfile::tempdir().unwrap();
    let hits = client(dir.path()).trending_tv().unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].media.douban_id.as_deref(), Some("36576514"));
    assert_eq!(hits[0].media.title, "深渊无间");
    assert_eq!(hits[0].media.kind, MediaKind::Tv);
    assert_eq!(hits[0].media.year, Some(2026));
    assert_eq!(hits[0].rating, Some(7.2));
    assert_eq!(
        hits[0].poster_path.as_deref(),
        Some("https://img9.doubanio.com/view/photo/s_ratio_poster/public/p2931762705.jpg")
    );
    assert_eq!(hits[0].overview.as_deref(), Some("深渊推理故事。"));

    assert_eq!(hits[1].media.douban_id.as_deref(), Some("36449295"));
    assert_eq!(hits[1].media.title, "兰香如故");
    assert_eq!(hits[1].media.year, Some(2026));
    assert_eq!(hits[1].rating, None); // value 0.0 is None
    assert_eq!(
        hits[1].poster_path.as_deref(),
        Some("https://img9.doubanio.com/view/photo/m_ratio_poster/public/p2933527614.jpg")
    );
}

#[test]
fn trending_movie_returns_pickable_media_from_collection() {
    let dir = tempfile::tempdir().unwrap();
    let hits = client(dir.path()).trending_movie().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].media.douban_id.as_deref(), Some("36850814"));
    assert_eq!(hits[0].media.title, "年会不能停！2");
    assert_eq!(hits[0].media.kind, MediaKind::Movie);
    assert_eq!(hits[0].media.year, Some(2026));
    assert_eq!(hits[0].rating, Some(6.6));
}

#[test]
fn now_playing_movie_returns_pickable_media_from_collection() {
    let dir = tempfile::tempdir().unwrap();
    let hits = client(dir.path()).now_playing_movie().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].media.douban_id.as_deref(), Some("35872123"));
    assert_eq!(hits[0].media.title, "敦煌英雄");
    assert_eq!(hits[0].media.kind, MediaKind::Movie);
    assert_eq!(hits[0].media.year, Some(2026));
    assert_eq!(hits[0].rating, Some(7.8));
}

#[test]
fn cache_hit_skips_second_http() {
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let douban = Douban::new(
        FakeHttp {
            bodies: bodies(),
            hits: hits.clone(),
        },
        &dir.path().join("catalog.db"),
    )
    .unwrap();
    douban.search_movie("matrix").unwrap();
    douban.search_movie("matrix").unwrap();
    assert_eq!(hits.lock().unwrap().len(), 1);
}

#[test]
fn missing_path_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let douban = Douban::new(
        FakeHttp {
            bodies: HashMap::new(),
            hits: Arc::new(Mutex::new(Vec::new())),
        },
        &dir.path().join("catalog.db"),
    )
    .unwrap();
    assert!(douban.search_movie("nope").is_err());
}

#[test]
fn douban_cache_does_not_collide_with_tmdb() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("catalog.db");
    let tmdb_hits = Arc::new(Mutex::new(Vec::new()));
    let douban_hits = Arc::new(Mutex::new(Vec::new()));
    let tmdb = Tmdb::new(
        FakeHttp {
            bodies: HashMap::from([(
                "/search/movie?query=matrix".into(),
                include_str!("fixtures/search_movie.json").into(),
            )]),
            hits: tmdb_hits.clone(),
        },
        &db,
    )
    .unwrap();
    let douban = Douban::new(
        FakeHttp {
            bodies: bodies(),
            hits: douban_hits.clone(),
        },
        &db,
    )
    .unwrap();
    assert_eq!(
        tmdb.search_movie("matrix").unwrap()[0]
            .media
            .tmdb_id
            .as_deref(),
        Some("603")
    );
    assert_eq!(
        douban.search_movie("matrix").unwrap()[0]
            .media
            .douban_id
            .as_deref(),
        Some("1291843")
    );
    tmdb.search_movie("matrix").unwrap();
    douban.search_movie("matrix").unwrap();
    assert_eq!(tmdb_hits.lock().unwrap().len(), 1);
    assert_eq!(douban_hits.lock().unwrap().len(), 1);
}
