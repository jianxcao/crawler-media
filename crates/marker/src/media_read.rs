//! Shared admission for background reads from rate-sensitive STRM sources.
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::{ProbeTarget, WorkControl};

const CANCELLATION_POLL: Duration = Duration::from_millis(25);
static REMOTE_READS: LazyLock<MediaReadGate> =
    LazyLock::new(|| MediaReadGate::new(Duration::from_secs(1)));

/// One remote read at a time across metadata, fingerprint and artwork workers.
/// Cooldown starts when the previous read finishes, including failed reads.
pub struct MediaReadGate {
    state: Mutex<State>,
    changed: Condvar,
    cooldown: Duration,
}

struct State {
    busy: bool,
    available_at: Instant,
    next_ticket: u64,
    waiting: VecDeque<u64>,
}

pub struct MediaReadPermit<'a> {
    gate: Option<&'a MediaReadGate>,
}

impl MediaReadGate {
    pub fn new(cooldown: Duration) -> Self {
        Self {
            state: Mutex::new(State {
                busy: false,
                available_at: Instant::now(),
                next_ticket: 0,
                waiting: VecDeque::new(),
            }),
            changed: Condvar::new(),
            cooldown,
        }
    }

    pub fn acquire(
        &self,
        path: &Path,
        control: &WorkControl,
    ) -> Result<MediaReadPermit<'_>, String> {
        if control.is_cancelled() {
            return Err("Library work cancelled".into());
        }
        if matches!(ProbeTarget::from_path(path), ProbeTarget::Local(_)) {
            return Ok(MediaReadPermit { gate: None });
        }
        let started = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let ticket = state.next_ticket;
        state.next_ticket = state.next_ticket.wrapping_add(1);
        state.waiting.push_back(ticket);
        loop {
            if control.is_cancelled() {
                state.waiting.retain(|waiting| *waiting != ticket);
                self.changed.notify_all();
                tracing::debug!(path = %path.display(), "Remote media read cancelled while waiting");
                return Err("Library work cancelled".into());
            }
            let now = Instant::now();
            if !state.busy && now >= state.available_at && state.waiting.front() == Some(&ticket) {
                state.waiting.pop_front();
                state.busy = true;
                tracing::debug!(path = %path.display(), waited_ms = started.elapsed().as_millis(),
                    cooldown_ms = self.cooldown.as_millis(), "Remote media read admitted");
                return Ok(MediaReadPermit { gate: Some(self) });
            }
            state = self
                .changed
                .wait_timeout(state, CANCELLATION_POLL)
                .unwrap_or_else(|poison| poison.into_inner())
                .0;
        }
    }

    /// Replaceable IO seam for deterministic offline callers and tests.
    pub fn read<T>(
        &self,
        path: &Path,
        control: &WorkControl,
        read: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _permit = self.acquire(path, control)?;
        read()
    }
}

impl Drop for MediaReadPermit<'_> {
    fn drop(&mut self) {
        if let Some(gate) = self.gate {
            let mut state = gate
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.busy = false;
            state.available_at = Instant::now() + gate.cooldown;
            gate.changed.notify_all();
        }
    }
}

/// All production background media reads use this process-wide gate.
/// Keep the permit until the child has exited and been reaped.
pub fn acquire_media_read(
    path: &Path,
    control: &WorkControl,
) -> Result<MediaReadPermit<'static>, String> {
    REMOTE_READS.acquire(path, control)
}
