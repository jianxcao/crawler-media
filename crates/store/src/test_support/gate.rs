//! Bounded cross-thread gates for tests.
//!
//! A bare [`std::sync::Barrier`] waits forever when the peer never arrives, so a
//! broken invariant looks like a hung CI run instead of a failure — a hang cannot
//! tell a deadlock from a slow machine. Every cross-thread rendezvous in these
//! tests goes through [`TimedGate`] instead: when the peer does not show up within
//! the gate's budget the waiting side panics, naming the gate.

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// How long a gate may stay unpaired before the waiting side fails the test.
///
/// Generous enough for a loaded CI box (the rendezvous is between two threads of
/// the same test, so a healthy pair crosses it in microseconds), and short enough
/// that a real deadlock is reported as a failure rather than as a hang.
pub const DEFAULT_GATE_TIMEOUT: Duration = Duration::from_secs(10);

/// Constructor for the two halves of a [`Gate`]. A gate is a rendezvous: each half
/// holds one end, and neither side returns from [`Gate::wait`] before the other has
/// arrived. The two halves may cross the same gate repeatedly.
pub struct TimedGate;

impl TimedGate {
    /// A two-party gate with the default budget.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(name: &'static str) -> (Arc<Gate>, Arc<Gate>) {
        Self::with_timeout(name, DEFAULT_GATE_TIMEOUT)
    }

    /// The same rendezvous with an explicit budget.
    ///
    /// A short budget is for the test that proves the timeout fires; production
    /// gates must stay long enough that a slow machine does not fail them.
    pub fn with_timeout(name: &'static str, timeout: Duration) -> (Arc<Gate>, Arc<Gate>) {
        // One channel per direction. Sharing a single one would make tokens
        // fungible across crossings, so a half could spend the peer's arrival for
        // the wrong rendezvous.
        let (left_tx, right_rx) = mpsc::channel();
        let (right_tx, left_rx) = mpsc::channel();
        (
            Arc::new(Gate {
                name,
                timeout,
                arrived_tx: left_tx,
                peer_arrived: Mutex::new(left_rx),
            }),
            Arc::new(Gate {
                name,
                timeout,
                arrived_tx: right_tx,
                peer_arrived: Mutex::new(right_rx),
            }),
        )
    }
}

/// One end of a [`TimedGate`] rendezvous.
pub struct Gate {
    name: &'static str,
    timeout: Duration,
    /// Signals this half's arrival to the other half.
    arrived_tx: mpsc::Sender<()>,
    /// Receives the other half's arrival.
    peer_arrived: Mutex<mpsc::Receiver<()>>,
}

impl Gate {
    /// Wait for the other end.
    ///
    /// # Panics
    ///
    /// When the other end never arrives within the gate's budget. The message names
    /// the gate, so a failing CI run says *which* handshake broke instead of
    /// timing out with no information at all.
    pub fn wait(&self) {
        // Announce first: the peer may be the one that is already waiting.
        let _ = self.arrived_tx.send(());
        let peer = self
            .peer_arrived
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if peer.recv_timeout(self.timeout).is_err() {
            panic!(
                "gate {} timed out after {:?}; the other side never arrived",
                self.name, self.timeout
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TimedGate;
    use std::time::Duration;

    /// Only one side ever arrives. A `Barrier` would block forever; the gate must
    /// fail the test instead, naming itself.
    #[test]
    #[should_panic(expected = "timed out")]
    fn unpaired_gate_panics_instead_of_hanging() {
        let (side, _never_used) = TimedGate::with_timeout("unpaired", Duration::from_millis(100));
        side.wait();
    }

    /// Both sides arrive: neither returns before the other has arrived, and the
    /// same pair can cross again (a pair is reused across two windows in the
    /// lease tests).
    #[test]
    fn paired_gate_crosses_in_both_directions() {
        let (left, right) = TimedGate::with_timeout("paired", Duration::from_millis(500));
        let worker = std::thread::spawn(move || {
            right.wait();
            right.wait();
        });
        left.wait();
        left.wait();
        worker.join().unwrap();
    }
}
