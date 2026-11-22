use std::sync::{Arc, Condvar, Mutex};

use domain::{TypedItemId, UserId};
use store::{Store, playback::session_ids::PlaybackRenewalError};

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

fn resolved(store: &Store, user: UserId, media: domain::MediaId) -> store::ResolvedResume {
    store
        .resolve_playback(user, &TypedItemId::Work(media), None, Some(42_000))
        .unwrap()
}

#[test]
fn concurrent_renewals_have_one_success_and_cannot_overwrite_its_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let user = UserId::new();
    super::support::seed_member(&store, user);
    let (media, ledger) =
        super::support::seed_published_file(&store, &tmp.path().join("library"), "renew");
    let original = super::support::issue_session(&store, user, "device", ledger, 1000);
    let resume = resolved(&store, user, media);
    let barrier = BoundedRendezvous::new(2);
    let results = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let (store, barrier, original, resume) = (
                    store.clone(),
                    barrier.clone(),
                    original.clone(),
                    resume.clone(),
                );
                scope.spawn(move || {
                    let mut guard = barrier.guard();
                    barrier.wait();
                    guard.disarm();
                    store.renew_play_session(user, "device", &original, &resume, 1001)
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(PlaybackRenewalError::Superseded)))
            .count(),
        1
    );
    let sid = results.into_iter().find_map(Result::ok).unwrap();
    let binding = store
        .play_session_binding(user, "device", &sid)
        .unwrap()
        .unwrap();
    assert_eq!(binding.generation, 2);
    assert!(store.activate_play_session(user, "device", &sid).unwrap());
    assert!(
        !store
            .activate_play_session(user, "device", &original)
            .unwrap()
    );
}

#[test]
fn unavailable_predecessor_cannot_issue_and_cleanup_preserves_generation_order() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    super::support::seed_member(&store, user);
    let (media, ledger) =
        super::support::seed_published_file(&store, &tmp.path().join("library"), "old");
    let original = super::support::issue_session(&store, user, "device", ledger, 1000);
    let resume = resolved(&store, user, media);
    let next = store
        .renew_play_session(user, "device", &original, &resume, 3_000_000)
        .unwrap();
    assert_eq!(
        store
            .play_session_binding(user, "device", &next)
            .unwrap()
            .unwrap()
            .generation,
        2
    );
    assert!(
        store
            .play_session_binding(user, "device", &original)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.renew_play_session(user, "device", &original, &resume, 3_000_001),
        Err(PlaybackRenewalError::PreviousUnavailable)
    ));
    let third = store
        .renew_play_session(user, "device", &next, &resume, 3_000_001)
        .unwrap();
    assert_eq!(
        store
            .play_session_binding(user, "device", &third)
            .unwrap()
            .unwrap()
            .generation,
        3
    );
}

#[test]
fn stale_resolved_source_rolls_back_and_leaves_predecessor_renewable() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    super::support::seed_member(&store, user);
    let (media, ledger) =
        super::support::seed_published_file(&store, &tmp.path().join("library"), "source");
    let original = super::support::issue_session(&store, user, "device", ledger, 1000);
    let current = resolved(&store, user, media);
    let mut stale = current.clone();
    stale.target.file.version.revision += 1;
    assert!(matches!(
        store.renew_play_session(user, "device", &original, &stale, 1001),
        Err(PlaybackRenewalError::Store(_))
    ));
    let next = store
        .renew_play_session(user, "device", &original, &current, 1001)
        .unwrap();
    assert_eq!(
        store
            .play_session_binding(user, "device", &next)
            .unwrap()
            .unwrap()
            .generation,
        2
    );
}

#[test]
fn stale_library_revision_cannot_renew_a_previously_resolved_source() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    super::support::seed_member(&store, user);
    let (media, ledger) =
        super::support::seed_published_file(&store, &tmp.path().join("library"), "revision");
    let original = super::support::issue_session(&store, user, "device", ledger, 1000);
    let mut stale = resolved(&store, user, media);
    stale.target.config_revision += 1;
    assert!(matches!(
        store.renew_play_session(user, "device", &original, &stale, 1001),
        Err(PlaybackRenewalError::Store(_))
    ));
    let current = resolved(&store, user, media);
    let next = store
        .renew_play_session(user, "device", &original, &current, 1001)
        .unwrap();
    assert_eq!(
        store
            .play_session_binding(user, "device", &next)
            .unwrap()
            .unwrap()
            .generation,
        2
    );
}
