//! One request's measurements. No credentials, SQL, or process-wide deltas.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use axum::{extract::Request, middleware::Next, response::Response};
use serde::Serialize;

use crate::provider::QueryTiming;
mod logging;

#[derive(Clone, Debug, Default, Serialize)]
pub struct QueryMetricsSnapshot {
    pub request_id: u64,
    pub auth_lock_wait_ms: f64,
    pub auth_ms: f64,
    pub credential_binding_ms: f64,
    pub queue_wait_ms: f64,
    pub prepare_lock_wait_ms: f64,
    pub prepare_lock_held_ms: f64,
    pub sql_ms: f64,
    pub cold_ms: f64,
    pub validation_lock_wait_ms: f64,
    pub validation_lock_held_ms: f64,
    pub elapsed_ms: f64,
    pub retry_count: u64,
    pub returned: Option<u64>,
    pub total: Option<u64>,
    pub vm_steps: Option<u64>,
    pub sort_operations: Option<u64>,
    pub fullscan_steps: Option<u64>,
    pub statements: Option<u64>,
    pub sqlite_profile_ms: Option<f64>,
    pub pool_rejections: u64,
    pub timeouts: u64,
    pub timeout_stage: Option<&'static str>,
    pub remaining_budget_ms: Option<f64>,
    pub worker_stop_delay_ms: Option<f64>,
    /// Durations still being measured by queued/running blocking workers.
    pub pending_phases: Vec<&'static str>,
}

#[derive(Default)]
struct Measurements {
    snapshot: QueryMetricsSnapshot,
    workers: Vec<(u64, &'static str)>,
    next_worker: u64,
    response_status: Option<u16>,
}

#[derive(Clone)]
pub struct QueryMetrics {
    data: Arc<Mutex<Measurements>>,
    timing: QueryTiming,
    diagnostic: bool,
}

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

tokio::task_local! { static CURRENT: QueryMetrics; }

impl QueryMetrics {
    pub fn new(timing: QueryTiming, diagnostic: bool) -> Self {
        Self {
            data: Arc::new(Mutex::new(Measurements {
                snapshot: QueryMetricsSnapshot {
                    request_id: NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
                    ..Default::default()
                },
                ..Default::default()
            })),
            timing,
            diagnostic,
        }
    }

    pub fn current() -> Option<Self> {
        CURRENT.try_with(Clone::clone).ok()
    }
    pub fn diagnostic(&self) -> bool {
        self.diagnostic
    }
    pub fn snapshot(&self) -> QueryMetricsSnapshot {
        let data = self.data.lock().unwrap_or_else(|p| p.into_inner());
        let mut snapshot = data.snapshot.clone();
        for (_, phase) in &data.workers {
            if !snapshot.pending_phases.contains(phase) {
                snapshot.pending_phases.push(phase);
            }
        }
        snapshot
    }
    pub fn update(&self, change: impl FnOnce(&mut QueryMetricsSnapshot)) {
        change(&mut self.data.lock().unwrap_or_else(|p| p.into_inner()).snapshot);
    }
    /// Register before dispatch so an HTTP timeout can observe queued work.
    pub fn worker_queued(&self) -> u64 {
        let mut data = self.data.lock().unwrap_or_else(|p| p.into_inner());
        data.next_worker += 1;
        let id = data.next_worker;
        data.workers.push((id, "queue"));
        id
    }
    pub fn worker_started(&self, id: u64, phase: &'static str, queued: Duration) {
        let mut data = self.data.lock().unwrap_or_else(|p| p.into_inner());
        data.snapshot.queue_wait_ms += queued.as_secs_f64() * 1000.;
        if let Some(worker) = data.workers.iter_mut().find(|w| w.0 == id) {
            worker.1 = phase;
        }
    }
    pub fn worker_phase(&self, id: u64, phase: &'static str) {
        let mut data = self.data.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(worker) = data.workers.iter_mut().find(|w| w.0 == id) {
            worker.1 = phase;
        }
    }
    pub fn worker_stopped(&self, id: u64, stage: &'static str, deadline: Instant) {
        let mut data = self.data.lock().unwrap_or_else(|p| p.into_inner());
        data.workers.retain(|w| w.0 != id);
        let status = data.response_status;
        let late = Instant::now() >= deadline;
        if late {
            data.snapshot.worker_stop_delay_ms = Some(
                Instant::now()
                    .saturating_duration_since(deadline)
                    .as_secs_f64()
                    * 1000.,
            );
        }
        drop(data);
        if late || status.is_some() {
            self.log(status, Some(stage));
        }
    }
    pub fn duration(&self, phase: &'static str, duration: Duration) {
        let ms = duration.as_secs_f64() * 1000.;
        self.update(|s| match phase {
            "auth" => s.auth_ms += ms,
            "auth_wait" => s.auth_lock_wait_ms += ms,
            "credential_binding" => s.credential_binding_ms += ms,
            "queue" => s.queue_wait_ms += ms,
            "prepare_wait" => s.prepare_lock_wait_ms += ms,
            "prepare_held" => s.prepare_lock_held_ms += ms,
            "validate_wait" => s.validation_lock_wait_ms += ms,
            "validate_held" => s.validation_lock_held_ms += ms,
            "sql" => s.sql_ms += ms,
            "cold" => s.cold_ms += ms,
            _ => {}
        });
    }
    pub fn page(returned: u64, total: Option<u64>) {
        if let Some(metrics) = Self::current() {
            metrics.update(|s| {
                s.returned = Some(returned);
                s.total = total;
            });
        }
    }
    pub fn retry() {
        if let Some(metrics) = Self::current() {
            metrics.update(|s| s.retry_count += 1);
        }
    }
    pub fn timeout(&self, stage: &'static str) {
        self.update(|s| {
            s.timeouts += 1;
            s.timeout_stage = Some(stage);
            s.remaining_budget_ms = Some(
                self.timing
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs_f64()
                    * 1000.,
            );
        });
    }
    fn complete(&self, status: u16) {
        let mut data = self.data.lock().unwrap_or_else(|p| p.into_inner());
        data.response_status = Some(status);
        data.snapshot.elapsed_ms = self.timing.started.elapsed().as_secs_f64() * 1000.;
        if status >= 400 {
            data.snapshot.returned = None;
            data.snapshot.total = None;
        }
        drop(data);
        self.log(Some(status), None);
    }
}

/// Both route families call this outside authentication, after installing timing.
pub async fn observe_request(mut request: Request, next: Next) -> Response {
    let Some(timing) = request.extensions().get::<QueryTiming>().copied() else {
        return next.run(request).await;
    };
    let metrics = request
        .extensions()
        .get::<QueryMetrics>()
        .cloned()
        .unwrap_or_else(|| QueryMetrics::new(timing, false));
    request.extensions_mut().insert(metrics.clone());
    let response = CURRENT.scope(metrics.clone(), next.run(request)).await;
    metrics.complete(response.status().as_u16());
    response
}
