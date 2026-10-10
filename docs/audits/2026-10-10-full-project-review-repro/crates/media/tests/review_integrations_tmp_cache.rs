use std::sync::{Arc, Mutex};
use media::{CatalogGet, Tmdb, TmdbError};

struct MutableReply(Arc<Mutex<String>>);
impl CatalogGet for MutableReply {
    fn get(&self, _: &str) -> Result<String, TmdbError> {
        Ok(self.0.lock().unwrap().clone())
    }
}

#[test]
fn malformed_response_must_not_replace_usable_stale_search_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let response = Arc::new(Mutex::new(include_str!("fixtures/search_movie.json").to_string()));
    let tmdb = Tmdb::new_at(MutableReply(response.clone()), &tmp.path().join("catalog.db"), 100).unwrap();
    let before = tmdb.search_movie("matrix").unwrap();
    tmdb.set_now(100 + media::DEFAULT_TTL_SECS + 1);
    *response.lock().unwrap() = "<html>Temporary proxy error</html>".into();
    assert!(tmdb.search_movie("matrix").is_err());
    *response.lock().unwrap() = include_str!("fixtures/search_movie.json").into();
    let after = tmdb.search_movie("matrix");
    assert!(after.is_ok(), "healthy upstream remains masked by malformed fresh cache: {after:?}");
    assert_eq!(after.unwrap().len(), before.len());
}
