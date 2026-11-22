//! Concurrent callers share finite upstream work and observe the same real result.
use domain::MediaKind;
use media::{CatalogGet, Tmdb, TmdbError};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
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
struct FiniteOrigin(AtomicBool);
impl CatalogGet for FiniteOrigin {
    fn get(&self, _: &str) -> Result<String, TmdbError> {
        if self.0.swap(false, Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(100));
            Ok(include_str!("fixtures/rich_movie_details.json").into())
        } else {
            Err(TmdbError::Http(
                "finite origin already serving one response".into(),
            ))
        }
    }
}
#[test]
fn simultaneous_cold_reads_share_a_complete_catalog_result() {
    let dir = tempfile::tempdir().unwrap();
    let tmdb = Arc::new(
        Tmdb::new_at(
            FiniteOrigin(AtomicBool::new(true)),
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
        let metadata=worker.join().unwrap().expect("coalesced callers must receive complete result instead of competing with finite origin");
        assert!(metadata.overview.is_some());
        assert!(!metadata.cast.is_empty());
    }
}

struct BoundedOrigin {
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}
impl CatalogGet for BoundedOrigin {
    fn get(&self, _: &str) -> Result<String, TmdbError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(100));
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(include_str!("fixtures/rich_movie_details.json").into())
    }
}
#[derive(Clone)]
struct SharedBoundedOrigin(Arc<BoundedOrigin>);
impl CatalogGet for SharedBoundedOrigin {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        self.0.get(path)
    }
}
#[test]
fn unrelated_cold_keys_keep_complete_results_under_a_fixed_upstream_bound() {
    // Candidate fixed source budget=8, with enough bounded pending capacity for16.
    // Parent may choose a smaller fixed bound; raising this assertion is not a fix.
    let dir = tempfile::tempdir().unwrap();
    let origin = Arc::new(BoundedOrigin {
        active: std::sync::atomic::AtomicUsize::new(0),
        peak: std::sync::atomic::AtomicUsize::new(0),
    });
    let tmdb = Arc::new(
        Tmdb::new_at(
            SharedBoundedOrigin(origin.clone()),
            &dir.path().join("cache.db"),
            1_000_000,
        )
        .unwrap(),
    );
    let ready = BoundedRendezvous::new(16);
    let workers: Vec<_> = (0..16)
        .map(|key| {
            let tmdb = tmdb.clone();
            let ready = ready.clone();
            std::thread::spawn(move || {
                let mut guard = ready.guard();
                ready.wait();
                guard.disarm();
                tmdb.details_with_meta(MediaKind::Movie, &key.to_string())
            })
        })
        .collect();
    for worker in workers {
        let metadata = worker
            .join()
            .unwrap()
            .expect("bounded queued cold reads retain complete results");
        assert!(metadata.overview.is_some());
        assert!(!metadata.cast.is_empty());
    }
    assert!(
        origin.peak.load(Ordering::SeqCst) <= 8,
        "different cold keys must not bypass the shared upstream capacity"
    );
}
