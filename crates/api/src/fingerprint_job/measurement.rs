//! One synchronous analysis invocation. No Store reads belong inside its CPU span.
use marker::FingerprintEngine;
use marker::matcher::session::{PairAnalysis, SeasonMatchSession};
use std::sync::Arc;

#[derive(Clone, Debug, Default, serde::Serialize, PartialEq, Eq)]
pub struct ComparisonMeasurement {
    pub analysis_policy_key: String,
    pub computed: u64,
    pub reused: u64,
    pub compare_ns: u64,
    pub refine_ns: u64,
}

pub struct EvidenceMeasurement {
    analysis: PairAnalysis,
    policy_key: String,
}

impl EvidenceMeasurement {
    pub fn new(session: Arc<SeasonMatchSession>, policy: &marker::SamplingPolicy) -> Self {
        let policy_key = super::analysis_policy_key(policy);
        Self {
            analysis: session.observe(policy_key.clone()),
            policy_key,
        }
    }

    pub fn measure<T>(
        self,
        analysis: impl FnOnce(&dyn FingerprintEngine) -> T,
    ) -> (T, ComparisonMeasurement) {
        let (result, work) = self.analysis.measure(analysis);
        (
            result,
            ComparisonMeasurement {
                analysis_policy_key: self.policy_key,
                computed: work.computed,
                reused: work.reused,
                compare_ns: work.compare_ns,
                refine_ns: work.refine_ns,
            },
        )
    }
}
