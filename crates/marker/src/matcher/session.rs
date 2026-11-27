//! Manager-wide raw pair reuse. No derived boundary, model or season decision is
//! retained here; every consumer must re-evaluate its current evidence.
mod key;
mod observation;

pub use observation::{PairAnalysis, PairWork};

use crate::{EpisodeEvidence, FingerprintEngine, PairFingerprintDecision};
use key::PairKey;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Instant,
};

pub const DEFAULT_MAX_PAIRS: usize = 10_000;
pub const DEFAULT_MAX_BYTES: usize = 32 * 1024 * 1024;
/// Raw comparison semantics, separate from capture and boundary policy versions.
pub const RAW_COMPARISON_ALGORITHM_VERSION: u32 = 1;

pub struct SeasonMatchSession {
    engine: Arc<dyn FingerprintEngine>,
    cache: Mutex<Cache>,
}

struct Cache {
    entries: BTreeMap<Arc<PairKey>, Entry>,
    order: BTreeMap<u128, Arc<PairKey>>,
    max_pairs: usize,
    max_bytes: usize,
    bytes: usize,
    clock: u128,
}

struct Entry {
    decision: PairFingerprintDecision,
    touched: u128,
    bytes: usize,
}

impl SeasonMatchSession {
    /// The immutable engine must obey typed/candidate projection consistency.
    /// Each argument order retains its own exact typed result; native matching
    /// may select different candidates when the arguments are reversed.
    pub fn new(engine: Arc<dyn FingerprintEngine>, max_pairs: usize, max_bytes: usize) -> Self {
        Self {
            engine,
            cache: Mutex::new(Cache {
                entries: BTreeMap::new(),
                order: BTreeMap::new(),
                max_pairs,
                max_bytes,
                bytes: 0,
                clock: 0,
            }),
        }
    }

    pub fn observe(self: &Arc<Self>, analysis_policy_key: String) -> PairAnalysis {
        PairAnalysis::new(self.clone(), analysis_policy_key)
    }

    fn compare(
        &self,
        first: &EpisodeEvidence,
        second: &EpisodeEvidence,
        policy: &str,
        min: f32,
        max: f32,
    ) -> (PairFingerprintDecision, PairWork) {
        let key = PairKey::new(first, second, policy, min, max);
        let cached = {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.clock += 1;
            let touched = cache.clock;
            if let Some(entry) = cache.entries.get_mut(&key) {
                let old_touched = entry.touched;
                entry.touched = touched;
                let decision = entry.decision.clone();
                if let Some(arc_key) = cache.order.remove(&old_touched) {
                    cache.order.insert(touched, arc_key);
                }
                Some(decision)
            } else {
                None
            }
        };
        if let Some(decision) = cached {
            (
                decision,
                PairWork {
                    reused: 1,
                    ..Default::default()
                },
            )
        } else {
            // Neither the cache nor observation lock encloses engine execution.
            // Simultaneous misses both execute and are honestly counted.
            let started = Instant::now();
            let decision = self.engine.compare_evidence(first, second, min, max);
            let compare_ns = started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
            self.cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, &decision);
            (
                decision,
                PairWork {
                    computed: 1,
                    compare_ns,
                    ..Default::default()
                },
            )
        }
    }
}

impl Cache {
    fn insert(&mut self, key: PairKey, decision: &PairFingerprintDecision) {
        if self.max_pairs == 0 || self.entries.contains_key(&key) {
            return;
        }
        // Charge Arc<PairKey> storage plus full BTreeMap node slots for both `entries`
        // and the `order` O(log N) LRU index, conservatively exceeding allocator overhead.
        let bookkeeping = size_of::<PairKey>()
            + 11 * (size_of::<Arc<PairKey>>()
                + size_of::<Entry>()
                + size_of::<u128>()
                + size_of::<Arc<PairKey>>())
            + 24 * size_of::<usize>()
            + 256;
        let dynamic = match decision {
            PairFingerprintDecision::Candidates(candidates) => candidates
                .capacity()
                .saturating_mul(size_of::<crate::CommonSegment>()),
            PairFingerprintDecision::Uncertain(reason) => reason.capacity(),
            PairFingerprintDecision::NoMatch => 0,
        };
        let bytes = bookkeeping
            .saturating_add(key.allocated_bytes())
            .saturating_add(dynamic);
        if bytes > self.max_bytes {
            return;
        }
        while self.entries.len() >= self.max_pairs || self.bytes > self.max_bytes - bytes {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes -= entry.bytes;
            }
        }
        self.clock += 1;
        let touched = self.clock;
        let shared_key = Arc::new(key);
        self.order.insert(touched, shared_key.clone());
        self.entries.insert(
            shared_key,
            Entry {
                decision: decision.clone(),
                touched,
                bytes,
            },
        );
        self.bytes += bytes;
    }
}
