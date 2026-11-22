use super::*;
use marker::{FingerprintEngine, PairFingerprintDecision};

fn pair(
    session: &Arc<SeasonMatchSession>,
    a: &EpisodeEvidence,
    b: &EpisodeEvidence,
) -> (PairFingerprintDecision, marker::matcher::session::PairWork) {
    session
        .observe("policy-1".into())
        .measure(|e| e.compare_evidence(a, b, 15., 240.))
}

#[test]
fn capacity_eviction_and_oversize_bypass_preserve_uncached_decisions() {
    let (engine, samples) = fixture(4);
    for (pairs, bytes) in [(1, 32 * 1024 * 1024), (10_000, 1), (0, 0)] {
        let session = Arc::new(SeasonMatchSession::new(engine.clone(), pairs, bytes));
        let fresh = Arc::new(SeasonMatchSession::new(engine.clone(), 0, 0));
        assert_eq!(analyze(&session, &samples).0, analyze(&fresh, &samples).0);
        let (again, work) = analyze(&session, &samples);
        assert_eq!(again, analyze(&fresh, &samples).0);
        assert!(
            work.computed > 12,
            "eviction/rejected entries must count repeated real work"
        );
    }
    let session = Arc::new(SeasonMatchSession::new(engine, 1, 32 * 1024 * 1024));
    assert_eq!(pair(&session, &samples[0], &samples[2]).1.computed, 1);
    assert_eq!(pair(&session, &samples[0], &samples[2]).1.reused, 1);
    pair(&session, &samples[0], &samples[4]);
    assert_eq!(pair(&session, &samples[0], &samples[2]).1.computed, 1);
}

struct LargeReason;
impl FingerprintEngine for LargeReason {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<marker::CommonSegment> {
        unreachable!()
    }
    fn compare_fingerprints(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> PairFingerprintDecision {
        PairFingerprintDecision::Uncertain("x".repeat(40_000))
    }
}

#[test]
fn byte_budget_charges_retained_reason_and_key_storage() {
    let (_, mut samples) = fixture(3);
    let session = Arc::new(SeasonMatchSession::new(
        Arc::new(LargeReason),
        10_000,
        64_000,
    ));
    let (decision, _) = pair(&session, &samples[0], &samples[2]);
    assert_eq!(pair(&session, &samples[0], &samples[2]).1.reused, 1);
    pair(&session, &samples[0], &samples[4]);
    let (recomputed, work) = pair(&session, &samples[0], &samples[2]);
    assert_eq!(decision, recomputed);
    assert_eq!(work.computed, 1);
    // Long source identity alone must also bypass a small global byte budget.
    samples[0].source_version = "v".repeat(65_000);
    pair(&session, &samples[0], &samples[2]);
    assert_eq!(pair(&session, &samples[0], &samples[2]).1.computed, 1);
}

#[test]
fn recently_used_pairs_survive_entry_eviction() {
    let (engine, samples) = fixture(3);
    let session = Arc::new(SeasonMatchSession::new(engine, 2, 32 * 1024 * 1024));
    pair(&session, &samples[0], &samples[2]);
    pair(&session, &samples[0], &samples[4]);
    assert_eq!(pair(&session, &samples[0], &samples[2]).1.reused, 1);
    pair(&session, &samples[2], &samples[4]);
    assert_eq!(pair(&session, &samples[0], &samples[2]).1.reused, 1);
    assert_eq!(pair(&session, &samples[0], &samples[4]).1.computed, 1);
}

#[test]
fn twenty_four_stable_episodes_require_only_1104_ordered_computations() {
    let (engine, samples) = fixture(24);
    let session = Arc::new(SeasonMatchSession::new(
        engine.clone(),
        10_000,
        32 * 1024 * 1024,
    ));
    let uncached = Arc::new(SeasonMatchSession::new(engine, 0, 0));
    let mut computed = 0;
    for count in 1..=24 {
        let current = &samples[..count * 2];
        let (incremental, work) = analyze(&session, current);
        assert_eq!(incremental, analyze(&uncached, current).0);
        computed += work.computed;
    }
    assert_eq!(computed, 1104);
    assert_eq!(analyze(&session, &samples).1.computed, 0);
}

struct RendezvousState {
    arrived: usize,
    aborted: bool,
}

struct BoundedRendezvous {
    parties: usize,
    state: std::sync::Mutex<RendezvousState>,
    changed: std::sync::Condvar,
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
            state: std::sync::Mutex::new(RendezvousState {
                arrived: 0,
                aborted: false,
            }),
            changed: std::sync::Condvar::new(),
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

struct ConcurrentEngine(Arc<BoundedRendezvous>);

struct LargeCandidates;
impl FingerprintEngine for LargeCandidates {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<marker::CommonSegment> {
        unreachable!()
    }
    fn compare_fingerprints(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> PairFingerprintDecision {
        PairFingerprintDecision::Candidates(vec![
            common_segment((30., 90.), (30., 90.), 0.5);
            4_000
        ])
    }
}

#[test]
fn oversized_candidate_vectors_bypass_storage_without_losing_candidates() {
    let (_, samples) = fixture(2);
    let session = Arc::new(SeasonMatchSession::new(
        Arc::new(LargeCandidates),
        10_000,
        64_000,
    ));
    let (cold, _) = pair(&session, &samples[0], &samples[2]);
    let (again, work) = pair(&session, &samples[0], &samples[2]);
    assert_eq!(again, cold);
    assert!(
        matches!(again, PairFingerprintDecision::Candidates(ref candidates) if candidates.len() == 4_000)
    );
    assert_eq!(work.computed, 1);
    assert_eq!(work.reused, 0);
}
impl FingerprintEngine for ConcurrentEngine {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<marker::CommonSegment> {
        unreachable!()
    }
    fn compare_fingerprints(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> PairFingerprintDecision {
        self.0.wait();
        PairFingerprintDecision::NoMatch
    }
}

#[test]
fn concurrent_duplicate_misses_are_attributed_to_each_actual_invocation() {
    let (_, samples) = fixture(2);
    let rendezvous = BoundedRendezvous::new(2);
    let session = Arc::new(SeasonMatchSession::new(
        Arc::new(ConcurrentEngine(rendezvous.clone())),
        10_000,
        32 * 1024 * 1024,
    ));
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            let mut guard = rendezvous.guard();
            let result = pair(&session, &samples[0], &samples[2]);
            guard.disarm();
            result
        });
        let second = scope.spawn(|| {
            let mut guard = rendezvous.guard();
            let result = pair(&session, &samples[0], &samples[2]);
            guard.disarm();
            result
        });
        for result in [first.join().unwrap(), second.join().unwrap()] {
            assert_eq!(result.0, PairFingerprintDecision::NoMatch);
            assert_eq!(result.1.computed, 1);
            assert_eq!(result.1.reused, 0);
        }
    });
    assert_eq!(pair(&session, &samples[0], &samples[2]).1.reused, 1);
}
