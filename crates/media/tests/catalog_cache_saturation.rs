//! A valid expired result survives exhaustion of bounded refresh admission.
use domain::MediaKind;
use media::{CatalogGet, Tmdb, TmdbError};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

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
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
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
    let dir = tempfile::tempdir().unwrap();
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
    let ready = BoundedRendezvous::new(66);
    let (done, results) = mpsc::channel();
    let workers: Vec<_> = (0..65)
        .map(|id| {
            let tmdb = tmdb.clone();
            let ready = ready.clone();
            let done = done.clone();
            std::thread::spawn(move || {
                let mut guard = ready.guard();
                ready.wait();
                guard.disarm();
                let result = tmdb.details_with_meta(MediaKind::Movie, &format!("cold-{id}"));
                done.send(result).unwrap();
            })
        })
        .collect();
    let mut main_guard = ready.guard();
    ready.wait();
    main_guard.disarm();
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
