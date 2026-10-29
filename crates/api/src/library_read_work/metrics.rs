use media_server::query_metrics::QueryMetrics;
use std::time::Instant;

pub(super) struct WorkerMetrics {
    metrics: Option<QueryMetrics>,
    stage: &'static str,
    deadline: Instant,
    held: Option<Instant>,
    queued: Option<Instant>,
    id: u64,
}
impl WorkerMetrics {
    pub(super) fn queued(
        metrics: Option<QueryMetrics>,
        stage: &'static str,
        deadline: Instant,
    ) -> Self {
        let id = metrics.as_ref().map_or(0, QueryMetrics::worker_queued);
        Self {
            metrics,
            stage,
            deadline,
            held: None,
            queued: Some(Instant::now()),
            id,
        }
    }
    pub(super) fn start(mut self) -> Self {
        if let Some(queued) = self.queued.take()
            && let Some(metrics) = &self.metrics
        {
            let phase = match self.stage {
                "lease" => "sql",
                "auth" => "auth_wait",
                "validate" => "validate_wait",
                "credential_binding" => "binding_wait",
                "cold" => "cold_worker",
                _ => "prepare_wait",
            };
            metrics.worker_started(self.id, phase, queued.elapsed());
        }
        self
    }
    pub(super) fn duration(&self, phase: &'static str, start: Instant) {
        if let Some(metrics) = &self.metrics {
            metrics.duration(phase, start.elapsed());
        }
    }
    pub(super) fn acquired(&mut self) {
        self.held = Some(Instant::now());
        if let Some(metrics) = &self.metrics {
            metrics.worker_phase(self.id, self.held_phase());
        }
    }
    fn held_phase(&self) -> &'static str {
        match self.stage {
            "validate" => "validate_held",
            "auth" | "credential_binding" => "other_held",
            _ => "prepare_held",
        }
    }
}
impl Drop for WorkerMetrics {
    fn drop(&mut self) {
        if let Some(start) = self.held {
            self.duration(self.held_phase(), start);
        }
        if let Some(metrics) = &self.metrics {
            if let Some(queued) = self.queued {
                metrics.duration("queue", queued.elapsed());
            }
            metrics.worker_stopped(self.id, self.stage, self.deadline);
        }
    }
}

pub(super) struct StageMetrics {
    metrics: Option<QueryMetrics>,
    stage: &'static str,
    started: Instant,
}
impl StageMetrics {
    pub(super) fn start(stage: &'static str) -> Self {
        Self {
            metrics: QueryMetrics::current(),
            stage,
            started: Instant::now(),
        }
    }
    pub(super) fn timeout(&self) {
        if let Some(m) = &self.metrics {
            m.timeout(self.stage);
        }
    }
    pub(super) fn rejected(&self) {
        if let Some(m) = &self.metrics {
            m.update(|s| s.pool_rejections += 1);
        }
    }
}
impl Drop for StageMetrics {
    fn drop(&mut self) {
        if let Some(m) = &self.metrics {
            m.duration(self.stage, self.started.elapsed());
        }
    }
}

pub(super) fn page_counts(result: &store::LibraryReadResult) -> (u64, Option<u64>) {
    match result {
        store::LibraryReadResult::ItemIndex(buckets) => (buckets.len() as u64, None),
        store::LibraryReadResult::Browse { page, .. } => (page.items.len() as u64, page.total),
        store::LibraryReadResult::Facets(facets) => (
            facets.groups.values().map(|v| v.len() as u64).sum(),
            Some(facets.total),
        ),
        store::LibraryReadResult::Protocol { page, .. } => {
            (page.items.len() as u64, Some(page.total))
        }
    }
}
