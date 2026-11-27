use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use plugin_api::{ErrorCode, Failure};
use plugin_host::Invocation;

use super::ports::api_err;

#[derive(Default)]
struct PluginDispatchEntry {
    closed_below_generation: i64,
    active: HashMap<String, Invocation>,
}

#[derive(Default)]
struct InFlightState {
    entries: HashMap<String, PluginDispatchEntry>,
    cancelled_libraries: HashSet<String>,
}

#[derive(Default, Clone)]
pub(crate) struct PluginMetricsSnapshot {
    pub total_invocations: u64,
    pub total_errors: u64,
    pub total_latency_ms: u64,
    pub max_latency_ms: u64,
}

pub(crate) struct InFlightRegistry {
    state: Mutex<InFlightState>,
    metrics: Mutex<HashMap<String, PluginMetricsSnapshot>>,
    changed: Condvar,
}

impl Default for InFlightRegistry {
    fn default() -> Self {
        Self {
            state: Mutex::new(InFlightState::default()),
            metrics: Mutex::new(HashMap::new()),
            changed: Condvar::new(),
        }
    }
}

pub(crate) struct InFlightGuard<'a> {
    registry: &'a InFlightRegistry,
    plugin_id: String,
    invocation_id: String,
    generation: i64,
    started: Instant,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.registry.finish_invocation(
            &self.plugin_id,
            &self.invocation_id,
            self.generation,
            self.started,
        );
    }
}

#[doc(hidden)]
pub struct OwnedInFlightGuard {
    registry: Arc<InFlightRegistry>,
    plugin_id: String,
    invocation_id: String,
    generation: i64,
    started: Instant,
}

impl Drop for OwnedInFlightGuard {
    fn drop(&mut self) {
        self.registry.finish_invocation(
            &self.plugin_id,
            &self.invocation_id,
            self.generation,
            self.started,
        );
    }
}

impl InFlightRegistry {
    fn finish_invocation(
        &self,
        plugin_id: &str,
        invocation_id: &str,
        generation: i64,
        started: Instant,
    ) {
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let record_metric = {
            let mut state = self.state.lock();
            if let Some(entry) = state.entries.get_mut(plugin_id) {
                entry.active.remove(invocation_id);
                generation >= entry.closed_below_generation
            } else {
                false
            }
        };
        if record_metric {
            let mut metrics = self.metrics.lock();
            let stat = metrics.entry(plugin_id.to_string()).or_default();
            stat.total_invocations = stat.total_invocations.saturating_add(1);
            stat.total_latency_ms = stat.total_latency_ms.saturating_add(elapsed_ms);
            stat.max_latency_ms = stat.max_latency_ms.max(elapsed_ms);
        }
        self.changed.notify_all();
    }

    fn insert_checked(
        &self,
        plugin_id: &str,
        generation: i64,
        inv: &Invocation,
    ) -> Result<(), Failure> {
        let mut state = self.state.lock();
        if let Some(lib_id) = inv.library_id.as_deref()
            && state.cancelled_libraries.contains(lib_id)
        {
            return Err(Failure::Cancelled);
        }
        let entry = state.entries.entry(plugin_id.to_string()).or_default();
        if generation < entry.closed_below_generation {
            return Err(Failure::Guest(api_err(
                ErrorCode::StaleGeneration,
                "plugin dispatch closed for draining",
            )));
        }
        entry.active.insert(inv.id().to_string(), inv.clone());
        Ok(())
    }

    pub(crate) fn try_enter(
        &self,
        plugin_id: &str,
        generation: i64,
        inv: &Invocation,
    ) -> Result<InFlightGuard<'_>, Failure> {
        self.insert_checked(plugin_id, generation, inv)?;
        Ok(InFlightGuard {
            registry: self,
            plugin_id: plugin_id.to_string(),
            invocation_id: inv.id().to_string(),
            generation,
            started: Instant::now(),
        })
    }

    pub(crate) fn try_enter_owned(
        self: &Arc<Self>,
        plugin_id: &str,
        generation: i64,
        inv: &Invocation,
    ) -> Result<OwnedInFlightGuard, Failure> {
        self.insert_checked(plugin_id, generation, inv)?;
        Ok(OwnedInFlightGuard {
            registry: self.clone(),
            plugin_id: plugin_id.to_string(),
            invocation_id: inv.id().to_string(),
            generation,
            started: Instant::now(),
        })
    }

    pub(crate) fn record_error(&self, plugin_id: &str) {
        let mut metrics = self.metrics.lock();
        let stat = metrics.entry(plugin_id.to_string()).or_default();
        stat.total_errors = stat.total_errors.saturating_add(1);
    }

    pub(crate) fn close_dispatch(&self, plugin_id: &str, min_open_generation: i64) {
        let to_cancel: Vec<Invocation> = {
            let mut state = self.state.lock();
            let entry = state.entries.entry(plugin_id.to_string()).or_default();
            entry.closed_below_generation = entry.closed_below_generation.max(min_open_generation);
            entry.active.values().cloned().collect()
        };
        for inv in to_cancel {
            inv.cancel();
        }
    }

    pub(crate) fn on_uninstall(&self, plugin_id: &str, min_open_generation: i64) {
        let to_cancel: Vec<Invocation> = {
            let mut state = self.state.lock();
            let entry = state.entries.entry(plugin_id.to_string()).or_default();
            entry.closed_below_generation = entry.closed_below_generation.max(min_open_generation);
            entry.active.values().cloned().collect()
        };
        self.metrics.lock().remove(plugin_id);
        for inv in to_cancel {
            inv.cancel();
        }
        self.changed.notify_all();
    }

    pub(crate) fn cancel_library(&self, library_id: &str) {
        let to_cancel: Vec<Invocation> = {
            let mut state = self.state.lock();
            state.cancelled_libraries.insert(library_id.to_string());
            state
                .entries
                .values()
                .flat_map(|entry| entry.active.values())
                .filter(|inv| inv.library_id.as_deref() == Some(library_id))
                .cloned()
                .collect()
        };
        for inv in to_cancel {
            inv.cancel();
        }
        self.changed.notify_all();
    }

    pub(crate) fn active_count(&self, plugin_id: &str) -> usize {
        self.state
            .lock()
            .entries
            .get(plugin_id)
            .map_or(0, |entry| entry.active.len())
    }

    pub(crate) fn wait_drained(&self, plugin_id: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut guard = self.state.lock();
        while guard
            .entries
            .get(plugin_id)
            .is_some_and(|e| !e.active.is_empty())
        {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let res = self.changed.wait_for(&mut guard, remaining);
            if res.timed_out()
                && guard
                    .entries
                    .get(plugin_id)
                    .is_some_and(|e| !e.active.is_empty())
            {
                return false;
            }
        }
        true
    }

    pub(crate) fn metrics_for(&self, plugin_id: &str) -> PluginMetricsSnapshot {
        self.metrics
            .lock()
            .get(plugin_id)
            .cloned()
            .unwrap_or_default()
    }
}
