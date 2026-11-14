use domain::MediaKind;
use media::cache::{CacheBackend, CatalogCache};
use media::response_cache::{self, CacheRow, CacheSource};
use media::{CatalogGet, CatalogRequest, Tmdb, TmdbError};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

const BODY: &str = include_str!("fixtures/movie_details.json");

struct Gate {
    entered: mpsc::Sender<String>,
    released: Mutex<bool>,
    ready: Condvar,
    calls: AtomicUsize,
    first_timeout: bool,
}

#[derive(Clone)]
struct Source(Arc<Gate>);

impl CatalogGet for Source {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        let call = self.0.calls.fetch_add(1, Ordering::SeqCst);
        let _ = self.0.entered.send(path.to_owned());
        let (released, _) = self
            .0
            .ready
            .wait_timeout_while(
                self.0.released.lock().unwrap(),
                Duration::from_secs(10),
                |value| !*value,
            )
            .unwrap();
        if !*released {
            return Err(TmdbError::Http("test origin hold expired".into()));
        }
        if self.0.first_timeout && call == 0 {
            Err(TmdbError::OriginTimeout)
        } else {
            Ok(BODY.to_owned())
        }
    }
}

fn gate(first_timeout: bool) -> (Arc<Gate>, mpsc::Receiver<String>) {
    let (entered, receiver) = mpsc::channel();
    (
        Arc::new(Gate {
            entered,
            released: Mutex::new(false),
            ready: Condvar::new(),
            calls: AtomicUsize::new(0),
            first_timeout,
        }),
        receiver,
    )
}

fn release(gate: &Gate) {
    *gate.released.lock().unwrap() = true;
    gate.ready.notify_all();
}

fn admission_is_inside_origin_budget() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (origin, entries) = gate(false);
    let tmdb = Arc::new(
        Tmdb::new_at(
            Source(origin.clone()),
            &dir.path().join("cache.db"),
            1_000_000,
        )
        .unwrap(),
    );
    release(&origin);
    let original = tmdb.details_poster(MediaKind::Movie, "603").unwrap();
    let warm_entry = entries.recv_timeout(Duration::from_secs(2));
    tmdb.set_now(1_000_000 + media::DEFAULT_TTL_SECS + 1);
    *origin.released.lock().unwrap() = false;
    let holders: Vec<_> = (0..8)
        .map(|id| {
            let tmdb = tmdb.clone();
            std::thread::spawn(move || tmdb.details_poster(MediaKind::Movie, &format!("held-{id}")))
        })
        .collect();
    let held: Vec<_> = (0..8)
        .map(|_| entries.recv_timeout(Duration::from_secs(2)))
        .collect();
    let (done, results) = mpsc::channel();
    let start = Instant::now();
    let targets: Vec<_> = ["unscoped", "long-caller", "603", "603"]
        .into_iter()
        .map(|key| {
            let tmdb = tmdb.clone();
            let done = done.clone();
            std::thread::spawn(move || {
                let run = || tmdb.details_poster(MediaKind::Movie, key);
                let result = if key == "long-caller" {
                    CatalogRequest::new(Instant::now() + Duration::from_secs(20)).scope(run)
                } else {
                    run()
                };
                let _ = done.send((key, result, start.elapsed()));
            })
        })
        .collect();
    let first = results.recv_timeout(Duration::from_millis(7900));
    let second = results.recv_timeout(Duration::from_millis(100));
    let third = results.recv_timeout(Duration::from_millis(100));
    let fourth = results.recv_timeout(Duration::from_millis(100));
    release(&origin);
    let holder_joins: Vec<_> = holders.into_iter().map(|thread| thread.join()).collect();
    let target_joins: Vec<_> = targets.into_iter().map(|thread| thread.join()).collect();
    let later_entries: Vec<_> = entries.try_iter().collect();

    assert!(warm_entry.is_ok());
    assert!(original.is_some());
    assert!(held.iter().all(Result::is_ok));
    for result in [first, second, third, fourth] {
        let (key, result, elapsed) = result.expect("origin admission must finish before release");
        if key == "603" {
            assert_eq!(result.unwrap(), original);
        } else {
            assert!(matches!(result, Err(TmdbError::OriginTimeout)));
        }
        assert!(elapsed < Duration::from_secs(8));
    }
    assert!(holder_joins.into_iter().all(|result| result.is_ok()));
    assert!(target_joins.into_iter().all(|result| result.is_ok()));
    assert!(
        !later_entries.iter().any(|path| path.contains("long-caller")
            || path.contains("unscoped")
            || path.contains("/movie/603?"))
    );
}

struct ObservedCache {
    conn: Mutex<rusqlite::Connection>,
    missed: mpsc::Sender<()>,
}

impl CacheBackend for ObservedCache {
    fn get(&self, source: CacheSource, key: &str) -> Result<Option<CacheRow>, TmdbError> {
        let row = response_cache::get(&self.conn.lock().unwrap(), source, key)?;
        if row.is_none() && std::thread::current().name() == Some("origin-follower") {
            let _ = self.missed.send(());
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

fn shared_origin_timeout_does_not_trigger_re_election() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (missed, misses) = mpsc::channel();
    let backend = Arc::new(ObservedCache {
        conn: Mutex::new(response_cache::open(dir.path().join("cache.db")).unwrap()),
        missed,
    });
    let cache = CatalogCache::with_backend(backend, CacheSource::Tmdb, 1_000_000);
    let (origin, entries) = gate(true);
    let tmdb = Arc::new(Tmdb::with_cache(Source(origin.clone()), cache));
    let leader_tmdb = tmdb.clone();
    let leader = std::thread::spawn(move || leader_tmdb.details_with_meta(MediaKind::Movie, "603"));
    let entered = entries.recv_timeout(Duration::from_secs(2));
    let follower_tmdb = tmdb.clone();
    let (done, results) = mpsc::channel();
    let follower = std::thread::Builder::new()
        .name("origin-follower".into())
        .spawn(move || {
            let _ = done.send(follower_tmdb.details_with_meta(MediaKind::Movie, "603"));
        })
        .unwrap();
    let miss = misses.recv_timeout(Duration::from_secs(2));
    let early = results.recv_timeout(Duration::from_millis(100));
    release(&origin);
    let leader_result = leader.join();
    let follower_result = results.recv_timeout(Duration::from_secs(2));
    let follower_join = follower.join();
    let cached = tmdb.cached_details_with_meta_for(MediaKind::Movie, "603", &["zh-CN"], &["CN"]);

    assert!(entered.is_ok() && miss.is_ok());
    assert!(matches!(early, Err(mpsc::RecvTimeoutError::Timeout)));
    assert!(matches!(leader_result, Ok(Err(TmdbError::OriginTimeout))));
    assert!(matches!(follower_result, Ok(Err(TmdbError::OriginTimeout))));
    assert!(follower_join.is_ok());
    assert!(matches!(cached, Ok(None)));
}

#[test]
fn origin_budget_covers_admission_and_shared_timeout_is_not_retried() {
    admission_is_inside_origin_budget();
    shared_origin_timeout_does_not_trigger_re_election();
}
