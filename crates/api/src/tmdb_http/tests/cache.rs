use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use domain::MediaKind;
use media::cache::{CacheBackend, CatalogCache};
use media::response_cache::{self, CacheRow, CacheSource};
use media::{Tmdb, TmdbError};
use ureq::{Body, config::IpFamily, http::Response};

use super::super::{retry::Runtime, transport::Transport};
use super::support::Fixture;

struct ObservedCache {
    conn: Mutex<rusqlite::Connection>,
    follower_miss: mpsc::Sender<()>,
}

impl CacheBackend for ObservedCache {
    fn get(&self, source: CacheSource, key: &str) -> Result<Option<CacheRow>, TmdbError> {
        let row = response_cache::get(&self.conn.lock().unwrap(), source, key)?;
        if row.is_none() && std::thread::current().name() == Some("follower") {
            let _ = self.follower_miss.send(());
        }
        Ok(row)
    }

    fn put(
        &self,
        source: CacheSource,
        key: &str,
        body: &str,
        fetched_at: i64,
        expires_at: Option<i64>,
    ) -> Result<(), TmdbError> {
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

struct Origin {
    gate: Mutex<Option<mpsc::Receiver<()>>>,
    entered: mpsc::Sender<()>,
    runtime: Arc<dyn Runtime>,
}

impl Transport for Origin {
    fn send(
        &self,
        _url: &str,
        _proxy: Option<&str>,
        _family: IpFamily,
        _timeout: Duration,
    ) -> Result<Response<Body>, ureq::Error> {
        if let Some(gate) = self.gate.lock().unwrap().take() {
            let _ = self.entered.send(());
            let _ = gate.recv_timeout(Duration::from_secs(2));
            self.runtime.sleep(Duration::from_secs(6));
            return Err(ureq::Error::Timeout(ureq::Timeout::Global));
        }
        Ok(Response::builder()
            .status(200)
            .body(
                Body::builder()
                    .data(r#"{"id":123,"title":"Recovered","poster_path":"/wrong.jpg"}"#),
            )
            .unwrap())
    }
}

#[test]
fn origin_timeout_is_shared_with_live_cache_follower() {
    let mut fixture = Fixture::new(Vec::new());
    let cache_dir = tempfile::tempdir().unwrap();
    let (miss_tx, miss_rx) = mpsc::channel();
    let backend = Arc::new(ObservedCache {
        conn: Mutex::new(response_cache::open(cache_dir.path().join("cache.db")).unwrap()),
        follower_miss: miss_tx,
    });
    let cache = CatalogCache::with_backend(backend, CacheSource::Tmdb, 1_000_000);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    fixture.http.transport = Arc::new(Origin {
        gate: Mutex::new(Some(release_rx)),
        entered: entered_tx,
        runtime: fixture.http.runtime.clone(),
    });
    let tmdb = Arc::new(Tmdb::with_cache(fixture.http, cache));
    let leader_tmdb = tmdb.clone();
    let leader = std::thread::spawn(move || leader_tmdb.details_poster(MediaKind::Movie, "123"));
    let entered = entered_rx.recv_timeout(Duration::from_secs(2));
    let follower_tmdb = tmdb.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let follower = std::thread::Builder::new()
        .name("follower".into())
        .spawn(move || {
            let _ = done_tx.send(follower_tmdb.details_poster(MediaKind::Movie, "123"));
        })
        .unwrap();
    let missed = miss_rx.recv_timeout(Duration::from_secs(2));
    let early = done_rx.recv_timeout(Duration::from_millis(100));
    let _ = release_tx.send(());
    let leader_result = leader.join();
    let follower_result = done_rx.recv_timeout(Duration::from_secs(2));
    let follower_join = follower.join();

    assert!(entered.is_ok());
    assert!(missed.is_ok());
    assert!(matches!(early, Err(mpsc::RecvTimeoutError::Timeout)));
    assert!(matches!(leader_result, Ok(Err(_))));
    assert!(matches!(follower_result, Ok(Err(_))));
    assert!(follower_join.is_ok());
    assert_eq!(
        tmdb.cached_details_poster(MediaKind::Movie, "123").unwrap(),
        None
    );
}
