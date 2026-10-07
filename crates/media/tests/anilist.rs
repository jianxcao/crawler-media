use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use domain::MediaKind;
use media::{Anilist, CatalogGet, TmdbError};

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
    "/search?query=matrix&format=MOVIE".into()
}

fn tv_path() -> String {
    "/search?query=cowboy&format=TV".into()
}

fn bodies() -> HashMap<String, String> {
    HashMap::from([
        (
            movie_path(),
            include_str!("fixtures/anilist_search_movie.json").into(),
        ),
        (
            tv_path(),
            include_str!("fixtures/anilist_search_tv.json").into(),
        ),
    ])
}

fn client(dir: &std::path::Path) -> Anilist<FakeHttp> {
    Anilist::new(
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
    assert_eq!(hits[0].media.anilist_id.as_deref(), Some("437"));
    assert!(hits[0].media.tmdb_id.is_none());
    assert_eq!(hits[0].media.title, "The Matrix");
    assert_eq!(hits[0].media.kind, MediaKind::Movie);
    assert_eq!(hits[0].media.year, Some(1999));
    assert_eq!(
        hits[0].poster_path.as_deref(),
        Some("https://s4.anilist.co/file/anilistcdn/media/anime/cover/medium/matrix.jpg")
    );
}

#[test]
fn search_tv_returns_pickable_media() {
    let dir = tempfile::tempdir().unwrap();
    let hits = client(dir.path()).search_tv("cowboy").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].media.anilist_id.as_deref(), Some("1"));
    assert_eq!(hits[0].media.kind, MediaKind::Tv);
    assert_eq!(hits[0].media.title, "Cowboy Bebop");
}

#[test]
fn cache_hit_skips_second_http() {
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let anilist = Anilist::new(
        FakeHttp {
            bodies: bodies(),
            hits: hits.clone(),
        },
        &dir.path().join("catalog.db"),
    )
    .unwrap();
    anilist.search_movie("matrix").unwrap();
    anilist.search_movie("matrix").unwrap();
    assert_eq!(hits.lock().unwrap().len(), 1);
}

#[test]
fn missing_path_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let anilist = Anilist::new(
        FakeHttp {
            bodies: HashMap::new(),
            hits: Arc::new(Mutex::new(Vec::new())),
        },
        &dir.path().join("catalog.db"),
    )
    .unwrap();
    assert!(anilist.search_movie("nope").is_err());
}
