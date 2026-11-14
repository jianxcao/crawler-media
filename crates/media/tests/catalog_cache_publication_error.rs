use domain::MediaKind;
use media::cache::{CacheBackend, CatalogCache};
use media::response_cache::{self, CacheRow, CacheSource};
use media::{CatalogGet, Tmdb, TmdbError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

struct Backend {
    conn: Mutex<rusqlite::Connection>,
    fail_put: AtomicBool,
}

impl CacheBackend for Backend {
    fn get(&self, source: CacheSource, key: &str) -> Result<Option<CacheRow>, TmdbError> {
        Ok(response_cache::get(
            &self.conn.lock().unwrap(),
            source,
            key,
        )?)
    }

    fn put(
        &self,
        source: CacheSource,
        key: &str,
        body: &str,
        fetched_at: i64,
        expires_at: Option<i64>,
    ) -> Result<(), TmdbError> {
        if self.fail_put.load(Ordering::SeqCst) {
            return Err(TmdbError::CacheBackend("publication unavailable".into()));
        }
        response_cache::put(
            &self.conn.lock().unwrap(),
            source,
            key,
            body,
            fetched_at,
            expires_at,
        )?;
        Ok(())
    }
}

struct Source(Arc<AtomicBool>);

impl CatalogGet for Source {
    fn get(&self, _: &str) -> Result<String, TmdbError> {
        let poster = if self.0.load(Ordering::SeqCst) {
            "/new.jpg"
        } else {
            "/old.jpg"
        };
        Ok(format!(r#"{{"id":603,"poster_path":"{poster}"}}"#))
    }
}

#[test]
fn failed_cache_publication_is_reported_and_preserves_stale_until_recovery() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let backend = Arc::new(Backend {
        conn: Mutex::new(response_cache::open(dir.path().join("cache.db")).unwrap()),
        fail_put: AtomicBool::new(false),
    });
    let new_source = Arc::new(AtomicBool::new(false));
    let cache = CatalogCache::with_backend(backend.clone(), CacheSource::Tmdb, 1_000_000);
    let tmdb = Tmdb::with_cache(Source(new_source.clone()), cache);
    assert_eq!(
        tmdb.details_poster(MediaKind::Movie, "603")
            .unwrap()
            .as_deref(),
        Some("/old.jpg")
    );

    tmdb.set_now(1_000_000 + media::DEFAULT_TTL_SECS + 1);
    new_source.store(true, Ordering::SeqCst);
    backend.fail_put.store(true, Ordering::SeqCst);
    let failed = tmdb.details_poster(MediaKind::Movie, "603");
    let retained = tmdb.cached_details_poster(MediaKind::Movie, "603");
    backend.fail_put.store(false, Ordering::SeqCst);
    let recovered = tmdb.details_poster(MediaKind::Movie, "603");

    assert!(
        matches!(failed, Err(TmdbError::CacheBackend(message)) if message == "publication unavailable")
    );
    assert_eq!(retained.unwrap().as_deref(), Some("/old.jpg"));
    assert_eq!(recovered.unwrap().as_deref(), Some("/new.jpg"));
}
