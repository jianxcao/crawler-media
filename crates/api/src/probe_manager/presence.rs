//! Presence commits durable lifecycle before waking the dispatch workers.
use store::AppliedLedgerPresence;

impl super::ProbeManager {
    /// Wake retirement/refill after actual committed generation changes.
    /// Dispatch identities remain owned by their worker until it retires them;
    /// removing an ID here could release a newer generation's reservation.
    pub(crate) fn presence_committed(&self, applied: &[AppliedLedgerPresence]) {
        if !applied.is_empty() {
            self.wake.notify_waiters();
        }
    }
}
