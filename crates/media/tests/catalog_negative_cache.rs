use std::sync::{Arc, Mutex};

use domain::MediaKind;
use media::{CatalogGet, Tmdb, TmdbError};

const NOW: i64 = 1_000_000;
const MISSING: &str = r#"{"success":false,"status_code":34,"status_message":"The resource you requested could not be found."}"#;
const PRESENT: &str = include_str!("fixtures/rich_movie_details.json");

#[test]
fn malformed_cached_similar_is_refreshable_without_hiding_source_failure() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("catalog.db");
    let source = Source(Arc::new(Mutex::new(Reply::Body(
        r#"{"results":[{"id":42,"title":"Recovered recommendation"}]}"#,
    ))));
    for broken in ["<html>broken cache</html>", r#"{"results":{}}"#] {
        let cache = media::cache::CatalogCache::open(&db, NOW).unwrap();
        cache
            .put("/movie/603/similar?language=zh-CN", broken)
            .unwrap();
        let tmdb = Tmdb::with_cache(source.clone(), cache);
        assert!(
            tmdb.cached_similar(MediaKind::Movie, "603")
                .unwrap()
                .is_none(),
            "invalid cached bytes must admit refresh instead of permanently returning 502"
        );
        let recovered = tmdb.similar(MediaKind::Movie, "603").unwrap();
        assert_eq!(recovered[0].media.title, "Recovered recommendation");
        assert!(
            tmdb.cached_similar(MediaKind::Movie, "603")
                .unwrap()
                .is_some()
        );
    }
    let cache = media::cache::CatalogCache::open(&db, NOW).unwrap();
    cache
        .put("/movie/604/similar?language=zh-CN", "invalid")
        .unwrap();
    let tmdb = Tmdb::with_cache(source.clone(), cache);
    *source.0.lock().unwrap() = Reply::Failure("502 origin unavailable");
    assert!(
        tmdb.cached_similar(MediaKind::Movie, "604")
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        tmdb.similar(MediaKind::Movie, "604"),
        Err(TmdbError::Http(_))
    ));
    assert!(
        tmdb.cached_similar(MediaKind::Movie, "604")
            .unwrap()
            .is_none()
    );
}

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

#[test]
fn cached_similar_distinguishes_unfetched_missing_and_complete_durable_results() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("catalog.db");
    let source = Source(Arc::new(Mutex::new(Reply::Body(
        r#"{"results":[{"id":42,"name":"Complete recommendation","first_air_date":"2026-01-01","poster_path":"/fixture.jpg"}]}"#,
    ))));
    let tmdb = Tmdb::new_at(source.clone(), &db, NOW).unwrap();
    assert!(tmdb.cached_similar(MediaKind::Tv, "603").unwrap().is_none());
    let complete = tmdb.similar(MediaKind::Tv, "603").unwrap();
    assert_eq!(complete.len(), 1);
    drop(tmdb);
    *source.0.lock().unwrap() = Reply::Failure("offline source must not be requested");
    let tmdb = Tmdb::new_at(source.clone(), &db, NOW + 1).unwrap();
    let cached = tmdb.cached_similar(MediaKind::Tv, "603").unwrap().unwrap();
    assert_eq!(cached.len(), complete.len());
    assert_eq!(cached[0].media.tmdb_id, complete[0].media.tmdb_id);
    assert_eq!(cached[0].media.title, complete[0].media.title);
    assert_eq!(cached[0].media.year, complete[0].media.year);
    assert_eq!(cached[0].poster_path, complete[0].poster_path);
    assert_eq!(cached[0].backdrop_path, complete[0].backdrop_path);
    assert_eq!(cached[0].rating, complete[0].rating);
    assert_eq!(cached[0].overview, complete[0].overview);
    assert!(tmdb.cached_similar(MediaKind::Tv, "604").unwrap().is_none());
    *source.0.lock().unwrap() = Reply::Body(MISSING);
    assert!(matches!(
        tmdb.similar(MediaKind::Tv, "604"),
        Err(TmdbError::NotFound(_))
    ));
    assert!(matches!(
        tmdb.cached_similar(MediaKind::Tv, "604"),
        Err(TmdbError::NotFound(_))
    ));
    tmdb.set_now(NOW + media::DEFAULT_TTL_SECS + 1);
    assert!(
        tmdb.cached_similar(MediaKind::Tv, "604").unwrap().is_none(),
        "expired negative cache cannot suppress source recovery forever"
    );
    assert!(
        tmdb.cached_similar(MediaKind::Tv, "timeout")
            .unwrap()
            .is_none()
    );
    *source.0.lock().unwrap() = Reply::Failure("timeout");
    assert!(tmdb.similar(MediaKind::Tv, "timeout").is_err());
    assert!(
        tmdb.cached_similar(MediaKind::Tv, "timeout")
            .unwrap()
            .is_none(),
        "transient failures must not become cached ready-empty results"
    );
}
