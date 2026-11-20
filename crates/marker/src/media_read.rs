//! Shared admission for background reads from rate-sensitive STRM sources.
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::{WorkControl, target::is_strm_path};

const CANCELLATION_POLL: Duration = Duration::from_millis(25);
/// Default remote reads in flight; instance probe settings may change this.
pub const DEFAULT_REMOTE_READ_LIMIT: usize = 1;
/// STRM sources: pause between remote reads.
pub const DEFAULT_REMOTE_READ_COOLDOWN: Duration = Duration::from_millis(500);
pub const READ_WAIT_STOPPED: &str = "Remote media read stopped before admission";
static REMOTE_READS: LazyLock<MediaReadGate> =
    LazyLock::new(|| MediaReadGate::new(DEFAULT_REMOTE_READ_LIMIT, DEFAULT_REMOTE_READ_COOLDOWN));

/// Bounded remote reads shared by metadata, fingerprint and artwork workers:
/// at most `limit` in flight, admitted FIFO. An optional cooldown starts when a
/// read finishes (including failed reads) and delays the next admission.
pub struct MediaReadGate {
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    active: usize,
    paused: bool,
    shutting_down: bool,
    limit: usize,
    cooldown: Duration,
    available_at: Instant,
    last_finished: Option<Instant>,
    next_ticket: u64,
    waiting: VecDeque<u64>,
}

pub struct MediaReadPermit<'a> {
    gate: Option<&'a MediaReadGate>,
}

impl MediaReadGate {
    pub fn new(limit: usize, cooldown: Duration) -> Self {
        Self {
            state: Mutex::new(State {
                active: 0,
                paused: false,
                shutting_down: false,
                limit: limit.max(1),
                cooldown,
                available_at: Instant::now(),
                last_finished: None,
                next_ticket: 0,
                waiting: VecDeque::new(),
            }),
            changed: Condvar::new(),
        }
    }

    /// Change the in-flight limit and cooldown; waiters re-check immediately.
    pub fn configure(&self, limit: usize, cooldown: Duration) {
        let mut state = self.lock();
        state.limit = limit.max(1);
        state.cooldown = cooldown;
        if let Some(finished) = state.last_finished {
            state.available_at = finished + cooldown;
        }
        self.changed.notify_all();
    }

    fn set_paused(&self, paused: bool) {
        self.lock().paused = paused;
        self.changed.notify_all();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn acquire(
        &self,
        path: &Path,
        control: &WorkControl,
    ) -> Result<MediaReadPermit<'_>, String> {
        if control.is_cancelled() {
            return Err("Library work cancelled".into());
        }
        // The STRM sidecar can change between admission and opening its target.
        if !is_strm_path(path) {
            return Ok(MediaReadPermit { gate: None });
        }
        let started = Instant::now();
        let mut state = self.lock();
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
            if state.shutting_down || state.paused || control.read_wait_stopped() {
                state.waiting.retain(|waiting| *waiting != ticket);
                self.changed.notify_all();
                return Err(READ_WAIT_STOPPED.into());
            }
            let now = Instant::now();
            if !state.paused
                && state.active < state.limit
                && now >= state.available_at
                && state.waiting.front() == Some(&ticket)
            {
                state.waiting.pop_front();
                state.active += 1;
                tracing::debug!(path = %path.display(), waited_ms = started.elapsed().as_millis(),
                    active = state.active, limit = state.limit, "Remote media read admitted");
                self.changed.notify_all();
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
            let mut state = gate.lock();
            state.active = state.active.saturating_sub(1);
            let finished = Instant::now();
            state.last_finished = Some(finished);
            state.available_at = finished + state.cooldown;
            gate.changed.notify_all();
        }
    }
}

/// Configure admission for subsequent reads without interrupting active reads.
pub fn configure_remote_reads(limit: usize, cooldown: Duration) {
    REMOTE_READS.configure(limit, cooldown);
}

/// Pause or resume admission while letting active reads complete.
pub fn set_remote_reads_paused(paused: bool) {
    REMOTE_READS.set_paused(paused);
}

/// Whether background remote-read admission is paused.
pub fn remote_reads_paused() -> bool {
    REMOTE_READS.lock().paused
}

/// Release background reads waiting at the gate as the process exits.
pub fn stop_remote_reads() {
    REMOTE_READS.lock().shutting_down = true;
    REMOTE_READS.changed.notify_all();
}

pub fn remote_reads_stopping() -> bool {
    REMOTE_READS.lock().shutting_down
}

/// All production background media reads use this process-wide gate.
/// Keep the permit until the child has exited and been reaped.
pub fn acquire_media_read(
    path: &Path,
    control: &WorkControl,
) -> Result<MediaReadPermit<'static>, String> {
    REMOTE_READS.acquire(path, control)
}
