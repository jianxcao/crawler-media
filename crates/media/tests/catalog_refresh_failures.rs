//! Shared refresh failures retain their category and do not prevent recovery.
use domain::MediaKind;
use media::{CatalogGet, Tmdb, TmdbError};
use std::sync::{
    Arc, Barrier,
    atomic::{AtomicUsize, Ordering},
};

struct FiniteFailure {
    state: AtomicUsize,
    missing: bool,
}
#[derive(Clone)]
struct Source(Arc<FiniteFailure>);
impl CatalogGet for Source {
    fn get(&self, _: &str) -> Result<String, TmdbError> {
        match self
            .0
            .state
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if self.0.missing {
                    Err(TmdbError::NotFound("real resource missing".into()))
                } else {
                    Err(TmdbError::Http("finite upstream failure".into()))
                }
            }
            Err(2) => Ok(include_str!("fixtures/rich_movie_details.json").into()),
            Err(_) => Err(TmdbError::Parse(
                "competing request must not consume source".into(),
            )),
        }
    }
}

#[test]
fn shared_failures_retain_not_found_or_transport_category_and_allow_recovery() {
    for missing in [false, true] {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let source = Arc::new(FiniteFailure {
            state: AtomicUsize::new(0),
            missing,
        });
        let tmdb = Arc::new(
            Tmdb::new_at(
                Source(source.clone()),
                &dir.path().join("cache.db"),
                1_000_000,
            )
            .unwrap(),
        );
        let ready = Arc::new(Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let tmdb = tmdb.clone();
                let ready = ready.clone();
                std::thread::spawn(move || {
                    ready.wait();
                    tmdb.details_with_meta(MediaKind::Movie, "603")
                })
            })
            .collect();
        for worker in workers {
            let error = worker.join().unwrap().unwrap_err();
            if missing {
                assert!(
                    matches!(error, TmdbError::NotFound(ref text) if text == "real resource missing")
                );
            } else {
                assert!(
                    matches!(error, TmdbError::Http(ref text) if text == "finite upstream failure")
                );
            }
        }
        source.state.store(2, Ordering::SeqCst);
        // A real negative remains negative until expiry; a transient never poisons cache.
        if missing {
            tmdb.set_now(1_000_000 + media::DEFAULT_TTL_SECS + 1);
        }
        let recovered = tmdb.details_with_meta(MediaKind::Movie, "603").unwrap();
        assert!(recovered.overview.is_some());
        assert!(!recovered.cast.is_empty());
    }
}
