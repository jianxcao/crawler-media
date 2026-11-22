//! Shared refresh failures retain their category and do not prevent recovery.
use domain::MediaKind;
use media::{CatalogGet, Tmdb, TmdbError};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct RendezvousState {
    arrived: usize,
    aborted: bool,
}

struct BoundedRendezvous {
    parties: usize,
    state: Mutex<RendezvousState>,
    changed: Condvar,
}

struct RendezvousGuard {
    rendezvous: Arc<BoundedRendezvous>,
    armed: bool,
}

impl RendezvousGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RendezvousGuard {
    fn drop(&mut self) {
        if self.armed {
            let mut guard = self
                .rendezvous
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            guard.aborted = true;
            self.rendezvous.changed.notify_all();
        }
    }
}

impl BoundedRendezvous {
    fn new(parties: usize) -> Arc<Self> {
        Arc::new(Self {
            parties,
            state: Mutex::new(RendezvousState {
                arrived: 0,
                aborted: false,
            }),
            changed: Condvar::new(),
        })
    }

    fn guard(self: &Arc<Self>) -> RendezvousGuard {
        RendezvousGuard {
            rendezvous: self.clone(),
            armed: true,
        }
    }

    fn wait(&self) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.aborted {
            drop(guard);
            panic!("rendezvous aborted");
        }
        guard.arrived += 1;
        if guard.arrived >= self.parties {
            self.changed.notify_all();
            return;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while guard.arrived < self.parties && !guard.aborted {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                guard.aborted = true;
                self.changed.notify_all();
                drop(guard);
                panic!("rendezvous timed out");
            };
            let (next, timeout) = self
                .changed
                .wait_timeout(guard, remaining)
                .unwrap_or_else(|e| e.into_inner());
            guard = next;
            if timeout.timed_out() && guard.arrived < self.parties && !guard.aborted {
                guard.aborted = true;
                self.changed.notify_all();
                drop(guard);
                panic!("rendezvous timed out");
            }
        }
        if guard.aborted {
            drop(guard);
            panic!("rendezvous aborted");
        }
    }
}

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
        let dir = tempfile::tempdir().unwrap();
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
        let ready = BoundedRendezvous::new(8);
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let tmdb = tmdb.clone();
                let ready = ready.clone();
                std::thread::spawn(move || {
                    let mut guard = ready.guard();
                    ready.wait();
                    guard.disarm();
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
