use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default)]
pub struct QueryPhaseSnapshot {
    pub lock_wait_ns: u64,
    pub selection_ns: u64,
    pub store_select_ns: u64,
    pub selected_facts_ns: u64,
    pub cold_details_ns: u64,
    pub dto_ns: u64,
    pub serialization_ns: u64,
}

#[derive(Default)]
pub(super) struct QueryPhaseTotals {
    lock_wait: AtomicU64,
    selection: AtomicU64,
    store_select: AtomicU64,
    selected_facts: AtomicU64,
    cold_details: AtomicU64,
    dto: AtomicU64,
    serialization: AtomicU64,
}

impl QueryPhaseTotals {
    pub(super) fn snapshot(&self) -> QueryPhaseSnapshot {
        QueryPhaseSnapshot {
            lock_wait_ns: self.lock_wait.load(Ordering::Relaxed),
            selection_ns: self.selection.load(Ordering::Relaxed),
            store_select_ns: self.store_select.load(Ordering::Relaxed),
            selected_facts_ns: self.selected_facts.load(Ordering::Relaxed),
            cold_details_ns: self.cold_details.load(Ordering::Relaxed),
            dto_ns: self.dto.load(Ordering::Relaxed),
            serialization_ns: self.serialization.load(Ordering::Relaxed),
        }
    }

    pub(super) fn record(&self, phase: &'static str, duration: Duration) {
        let ns = duration.as_nanos().min(u128::from(u64::MAX)) as u64;
        let counter = match phase {
            "lock_wait" => &self.lock_wait,
            "selection" => &self.selection,
            "store_select" => &self.store_select,
            "selected_facts" => &self.selected_facts,
            "cold_details" => &self.cold_details,
            "dto" => &self.dto,
            "serialization" => &self.serialization,
            _ => return,
        };
        counter.fetch_add(ns, Ordering::Relaxed);
    }
}
