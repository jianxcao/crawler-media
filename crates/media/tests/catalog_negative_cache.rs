use std::sync::{Arc, Mutex};

use domain::MediaKind;
use media::{CatalogGet, Tmdb, TmdbError};

const NOW: i64 = 1_000_000;
const MISSING: &str = r#"{"success":false,"status_code":34,"status_message":"The resource you requested could not be found."}"#;
const PRESENT: &str = include_str!("fixtures/rich_movie_details.json");

#[derive(Clone)]
enum Reply {
    Body(&'static str),
    Failure(&'static str),
}

#[derive(Clone)]
struct Source(Arc<Mutex<Reply>>);

impl CatalogGet for Source {
    fn get(&self, _: &str) -> Result<String, TmdbError> {
        match &*self.0.lock().unwrap() {
            Reply::Body(body) => Ok((*body).into()),
            Reply::Failure(message) => Err(TmdbError::Http((*message).into())),
        }
    }
}

#[test]
fn catalog_not_found_is_cached_but_transient_errors_are_not() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("catalog.db");
    let source = Source(Arc::new(Mutex::new(Reply::Body(MISSING))));
    let tmdb = Tmdb::new_at(source.clone(), &db, NOW).unwrap();
    let missing = tmdb.details_with_meta(MediaKind::Movie, "603").unwrap_err();
    assert!(matches!(missing, TmdbError::NotFound(_)), "{missing:?}");
    drop(tmdb);

    *source.0.lock().unwrap() = Reply::Body(PRESENT);
    let reopened = Tmdb::new_at(source.clone(), &db, NOW + 1).unwrap();
    let cached_missing = reopened
        .details_with_meta(MediaKind::Movie, "603")
        .unwrap_err();
    assert!(matches!(cached_missing, TmdbError::NotFound(_)));
    reopened.set_now(NOW + media::DEFAULT_TTL_SECS + 1);
    let recovered = reopened.details_with_meta(MediaKind::Movie, "603").unwrap();
    assert!(recovered.overview.is_some());
    drop(reopened);

    for failure in ["timeout", "401 unauthorized", "503 unavailable"] {
        *source.0.lock().unwrap() = Reply::Failure(failure);
        let tmdb = Tmdb::new_at(source.clone(), &db, NOW).unwrap();
        let error = tmdb
            .details_with_meta(MediaKind::Movie, failure)
            .unwrap_err();
        assert!(matches!(error, TmdbError::Http(_)));
        *source.0.lock().unwrap() = Reply::Body(PRESENT);
        assert!(tmdb.details_with_meta(MediaKind::Movie, failure).is_ok());
    }
}

#[test]
fn malformed_catalog_response_does_not_poison_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let source = Source(Arc::new(Mutex::new(Reply::Body(
        "<html>proxy failure</html>",
    ))));
    let tmdb = Tmdb::new_at(source.clone(), &dir.path().join("catalog.db"), NOW).unwrap();
    assert!(tmdb.details_with_meta(MediaKind::Movie, "603").is_err());
    *source.0.lock().unwrap() = Reply::Body(PRESENT);
    assert!(tmdb.details_with_meta(MediaKind::Movie, "603").is_ok());
}

#[test]
fn similar_missing_is_durable_and_expires_without_hiding_transient_errors() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("catalog.db");
    let source = Source(Arc::new(Mutex::new(Reply::Body(MISSING))));
    let tmdb = Tmdb::new_at(source.clone(), &db, NOW).unwrap();
    assert!(tmdb.similar(MediaKind::Movie, "603").is_err());
    drop(tmdb);
    *source.0.lock().unwrap() = Reply::Body(r#"{"results":[]}"#);
    let tmdb = Tmdb::new_at(source.clone(), &db, NOW + 1).unwrap();
    assert!(tmdb.similar(MediaKind::Movie, "603").is_err());
    tmdb.set_now(NOW + media::DEFAULT_TTL_SECS + 1);
    *source.0.lock().unwrap() = Reply::Failure("timeout");
    assert!(matches!(
        tmdb.similar(MediaKind::Movie, "603"),
        Err(TmdbError::Http(_))
    ));
    *source.0.lock().unwrap() = Reply::Body(r#"{"results":[]}"#);
    assert!(tmdb.similar(MediaKind::Movie, "603").unwrap().is_empty());
}
