use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use domain::MediaKind;
use media::{Bangumi, CatalogGet, TmdbError};

struct FakeHttp {
    bodies: HashMap<String, String>,
    hits: Arc<Mutex<Vec<String>>>,
}

impl CatalogGet for FakeHttp {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        self.hits.lock().unwrap().push(path.to_string());
        self.bodies
            .get(path)
            .cloned()
            .ok_or_else(|| TmdbError::Http(format!("missing {path}")))
    }
}

fn movie_path() -> String {
    "/search/subject/matrix?type=6&responseGroup=small".into()
}

fn tv_path() -> String {
    "/search/subject/cowboy?type=2&responseGroup=small".into()
}

fn bodies() -> HashMap<String, String> {
    HashMap::from([
        (
            movie_path(),
            include_str!("fixtures/bangumi_search_movie.json").into(),
        ),
        (
            tv_path(),
            include_str!("fixtures/bangumi_search_tv.json").into(),
        ),
    ])
}

fn client(dir: &std::path::Path) -> Bangumi<FakeHttp> {
    Bangumi::new(
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
    assert_eq!(hits[0].media.bangumi_id.as_deref(), Some("1040"));
    assert!(hits[0].media.tmdb_id.is_none());
    assert_eq!(hits[0].media.title, "黑客帝国");
    assert_eq!(hits[0].media.kind, MediaKind::Movie);
    assert_eq!(hits[0].media.year, Some(1999));
    assert_eq!(
        hits[0].poster_path.as_deref(),
        Some("https://lain.bgm.tv/pic/cover/l/matrix.jpg")
    );
}

#[test]
fn search_tv_returns_pickable_media() {
    let dir = tempfile::tempdir().unwrap();
    let hits = client(dir.path()).search_tv("cowboy").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].media.bangumi_id.as_deref(), Some("253"));
    assert_eq!(hits[0].media.kind, MediaKind::Tv);
    assert_eq!(hits[0].media.title, "星际牛仔");
}

#[test]
fn cache_hit_skips_second_http() {
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let bangumi = Bangumi::new(
        FakeHttp {
            bodies: bodies(),
            hits: hits.clone(),
        },
        &dir.path().join("catalog.db"),
    )
    .unwrap();
    bangumi.search_movie("matrix").unwrap();
    bangumi.search_movie("matrix").unwrap();
    assert_eq!(hits.lock().unwrap().len(), 1);
}

#[test]
fn missing_path_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let bangumi = Bangumi::new(
        FakeHttp {
            bodies: HashMap::new(),
            hits: Arc::new(Mutex::new(Vec::new())),
        },
        &dir.path().join("catalog.db"),
    )
    .unwrap();
    assert!(bangumi.search_movie("nope").is_err());
}
