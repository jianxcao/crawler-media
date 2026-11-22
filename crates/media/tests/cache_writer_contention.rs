//! C03: concurrent first opens of one cache file must not lose schema or data.

use std::sync::{Arc, Condvar, Mutex};

use media::response_cache::{self, CacheSource};

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

#[test]
fn concurrent_first_open_and_put_keep_schema_and_data() {
    for round in 0..20 {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("catalog.db");
        let barrier = BoundedRendezvous::new(8);
        let workers: Vec<_> = (0..8)
            .map(|index| {
                let (path, barrier) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let mut guard = barrier.guard();
                    barrier.wait();
                    guard.disarm();
                    let conn = response_cache::open(&path)?;
                    response_cache::put(
                        &conn,
                        CacheSource::Tmdb,
                        &format!("key-{index}"),
                        &format!("value-{index}"),
                        1,
                        None,
                    )
                })
            })
            .collect();
        for (index, worker) in workers.into_iter().enumerate() {
            let outcome = worker.join().unwrap();
            assert!(outcome.is_ok(), "round {round} worker {index}: {outcome:?}");
        }
        let conn = response_cache::open(&path).unwrap();
        for index in 0..8 {
            let committed_cache_value =
                response_cache::get(&conn, CacheSource::Tmdb, &format!("key-{index}"))
                    .unwrap()
                    .map(|row| row.body);
            assert_eq!(committed_cache_value, Some(format!("value-{index}")));
        }
    }
}
