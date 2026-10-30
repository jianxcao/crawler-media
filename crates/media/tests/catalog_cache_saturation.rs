//! A valid expired result survives exhaustion of bounded refresh admission.
use domain::MediaKind;
use media::{CatalogGet, Tmdb, TmdbError};
use std::sync::{Arc, Barrier, Condvar, Mutex, mpsc};
use std::time::Duration;

#[derive(Default)]
struct Origin {
    released: Mutex<bool>,
    ready: Condvar,
}
#[derive(Clone)]
struct Source(Arc<Origin>);
impl CatalogGet for Source {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        if path.starts_with("/movie/603?") {
            return Ok(include_str!("fixtures/rich_movie_details.json").into());
        }
        // Every held request terminates even if the assertion below fails.
        let (released, _) = self
            .0
            .ready
            .wait_timeout_while(
                self.0.released.lock().unwrap(),
                Duration::from_secs(1),
                |released| !*released,
            )
            .unwrap();
        if !*released {
            return Err(TmdbError::Http("finite origin hold expired".into()));
        }
        Ok(include_str!("fixtures/rich_movie_details.json").into())
    }
}

#[test]
fn expired_complete_catalog_result_survives_saturated_refresh_admission() {
    let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let origin = Arc::new(Origin::default());
    let tmdb = Arc::new(
        Tmdb::new_at(
            Source(origin.clone()),
            &dir.path().join("cache.db"),
            1_000_000,
        )
        .unwrap(),
    );
    let original = tmdb.details_with_meta(MediaKind::Movie, "603").unwrap();
    assert!(original.overview.is_some());
    assert!(!original.cast.is_empty());
    tmdb.set_now(1_000_000 + media::DEFAULT_TTL_SECS + 1);
    let ready = Arc::new(Barrier::new(66));
    let (done, results) = mpsc::channel();
    let workers: Vec<_> = (0..65)
        .map(|id| {
            let tmdb = tmdb.clone();
            let ready = ready.clone();
            let done = done.clone();
            std::thread::spawn(move || {
                ready.wait();
                let result = tmdb.details_with_meta(MediaKind::Movie, &format!("cold-{id}"));
                done.send(result).unwrap();
            })
        })
        .collect();
    ready.wait();
    // A public overload response establishes that all64admissions are occupied;
    // unlike a sleep, this does not assume queued callers have been scheduled.
    let overflow = results.recv_timeout(Duration::from_millis(500));
    let stale = if matches!(&overflow, Ok(Err(TmdbError::Http(message))) if message.contains("capacity exhausted"))
    {
        Some(tmdb.details_with_meta(MediaKind::Movie, "603"))
    } else {
        None
    };
    *origin.released.lock().unwrap() = true;
    origin.ready.notify_all();
    for worker in workers {
        worker.join().unwrap();
    }
    assert!(
        matches!(&overflow, Ok(Err(TmdbError::Http(message))) if message.contains("capacity exhausted")),
        "finite cold requests must establish saturation before stale read"
    );
    let stale = stale
        .unwrap()
        .expect("valid stale data remains available when refresh capacity is full");
    assert_eq!(stale.overview, original.overview);
    assert_eq!(stale.cast, original.cast);
}
