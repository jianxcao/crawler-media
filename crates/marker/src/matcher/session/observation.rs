use super::SeasonMatchSession;
use crate::{CommonSegment, EpisodeEvidence, FingerprintEngine, PairFingerprintDecision};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};

/// Work attributed directly to this invocation, never derived by subtracting
/// concurrent manager-global totals. Cache lookup overhead belongs to refine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PairWork {
    pub computed: u64,
    pub reused: u64,
    pub compare_ns: u64,
    pub refine_ns: u64,
}

pub struct PairAnalysis {
    session: Arc<SeasonMatchSession>,
    policy: String,
    observation: Mutex<PairWork>,
}

impl PairAnalysis {
    pub(super) fn new(session: Arc<SeasonMatchSession>, policy: String) -> Self {
        Self {
            session,
            policy,
            observation: Mutex::new(PairWork::default()),
        }
    }

    /// Synchronous CPU span only. Chapter reads and Store leases belong outside.
    /// Raw engine time is excluded from the remaining processing wall span.
    pub fn measure<T>(self, analysis: impl FnOnce(&dyn FingerprintEngine) -> T) -> (T, PairWork) {
        let started = Instant::now();
        let result = analysis(&self);
        let elapsed = started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        let mut work = self
            .observation
            .into_inner()
            .unwrap_or_else(|e| e.into_inner());
        work.refine_ns = elapsed.saturating_sub(work.compare_ns);
        (result, work)
    }
}

impl FingerprintEngine for PairAnalysis {
    fn compare_evidence(
        &self,
        first: &EpisodeEvidence,
        second: &EpisodeEvidence,
        min: f32,
        max: f32,
    ) -> PairFingerprintDecision {
        let (decision, work) = self.session.compare(first, second, &self.policy, min, max);
        let mut observation = self.observation.lock().unwrap_or_else(|e| e.into_inner());
        observation.computed += work.computed;
        observation.reused += work.reused;
        observation.compare_ns = observation.compare_ns.saturating_add(work.compare_ns);
        decision
    }
    fn extract_at(
        &self,
        path: &Path,
        start: u32,
        duration: u32,
        control: &crate::WorkControl,
    ) -> Result<Vec<u32>, String> {
        self.session
            .engine
            .extract_at(path, start, duration, control)
    }
    // Slice-only convenience APIs lack source identity and stay uncached.
    fn compare_fingerprints(
        &self,
        a: &[u32],
        b: &[u32],
        min: f32,
        max: f32,
    ) -> PairFingerprintDecision {
        self.session.engine.compare_fingerprints(a, b, min, max)
    }
    fn find_common_segment(
        &self,
        a: &[u32],
        b: &[u32],
        min: f32,
        max: f32,
    ) -> Option<CommonSegment> {
        self.session.engine.find_common_segment(a, b, min, max)
    }
    fn find_common_segments(&self, a: &[u32], b: &[u32], min: f32, max: f32) -> Vec<CommonSegment> {
        self.session.engine.find_common_segments(a, b, min, max)
    }
}
