use domain::MediaKind;
use media::cache::{CacheBackend, CatalogCache};
use media::response_cache::{self, CacheRow, CacheSource};
use media::{CatalogGet, CatalogRequest, Tmdb, TmdbError};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

struct ObservedCache {
    conn: Mutex<rusqlite::Connection>,
    follower_miss: mpsc::Sender<()>,
}

impl CacheBackend for ObservedCache {
    fn get(&self, source: CacheSource, key: &str) -> Result<Option<CacheRow>, TmdbError> {
        let result = response_cache::get(&self.conn.lock().unwrap(), source, key)?;
        if result.is_none() && std::thread::current().name() == Some("follower") {
            let _ = self.follower_miss.send(());
        }
        Ok(result)
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
    entered: mpsc::Sender<()>,
    released: Mutex<bool>,
    ready: Condvar,
    active: AtomicUsize,
    overlap: AtomicBool,
}

#[derive(Clone)]
struct Source(Arc<Origin>);

impl CatalogGet for Source {
    fn get(&self, _: &str) -> Result<String, TmdbError> {
        if self.0.active.fetch_add(1, Ordering::SeqCst) != 0 {
            self.0.overlap.store(true, Ordering::SeqCst);
        }
        let _ = self.0.entered.send(());
        let (released, _) = self
            .0
            .ready
            .wait_timeout_while(
                self.0.released.lock().unwrap(),
                Duration::from_secs(5),
                |value| !*value,
            )
            .unwrap();
        let result = if !*released {
            Err(TmdbError::Http("test origin timed out".into()))
        } else if let Some(request) = CatalogRequest::current() {
            request
                .check()
                .map(|_| include_str!("fixtures/rich_movie_details.json").to_owned())
        } else {
            Ok(include_str!("fixtures/rich_movie_details.json").to_owned())
        };
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        result
    }
}

#[test]
fn cancelling_leader_allows_live_follower_to_retry_shared_refresh() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (miss_tx, miss_rx) = mpsc::channel();
    let backend = Arc::new(ObservedCache {
        conn: Mutex::new(response_cache::open(dir.path().join("cache.db")).unwrap()),
        follower_miss: miss_tx,
    });
    let cache = CatalogCache::with_backend(backend, CacheSource::Tmdb, 1_000_000);
    let (entered_tx, entered_rx) = mpsc::channel();
    let origin = Arc::new(Origin {
        entered: entered_tx,
        released: Mutex::new(false),
        ready: Condvar::new(),
        active: AtomicUsize::new(0),
        overlap: AtomicBool::new(false),
    });
    let tmdb = Arc::new(Tmdb::with_cache(Source(origin.clone()), cache));
    let request = CatalogRequest::new(Instant::now() + Duration::from_secs(5));
    let leader_request = request.clone();
    let leader_tmdb = tmdb.clone();
    let leader = std::thread::spawn(move || {
        leader_request.scope(|| leader_tmdb.details_with_meta(MediaKind::Movie, "603"))
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(2));
    let follower_tmdb = tmdb.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let follower = std::thread::Builder::new()
        .name("follower".into())
        .spawn(move || {
            let result = follower_tmdb.details_with_meta(MediaKind::Movie, "603");
            let _ = done_tx.send(result);
        })
        .unwrap();
    let missed = miss_rx.recv_timeout(Duration::from_secs(2));
    let early = done_rx.recv_timeout(Duration::from_millis(100));
    request.cancel();
    *origin.released.lock().unwrap() = true;
    origin.ready.notify_all();
    let leader_result = leader.join();
    let follower_result = done_rx.recv_timeout(Duration::from_secs(2));
    let follower_join = follower.join();

    assert!(entered.is_ok());
    assert!(missed.is_ok());
    assert!(matches!(early, Err(mpsc::RecvTimeoutError::Timeout)));
    assert!(matches!(leader_result, Ok(Err(TmdbError::Cancelled))));
    assert!(follower_join.is_ok());
    let metadata = follower_result.unwrap().unwrap();
    assert!(metadata.overview.is_some());
    assert!(!metadata.cast.is_empty());
    assert!(!origin.overlap.load(Ordering::SeqCst));
}
