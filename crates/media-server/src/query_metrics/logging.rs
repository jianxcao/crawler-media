use super::QueryMetrics;

impl QueryMetrics {
    pub(super) fn log(&self, status: Option<u16>, worker_stage: Option<&'static str>) {
        let s = self.snapshot();
        let measured = |phase, value| (!s.pending_phases.contains(&phase)).then_some(value);
        macro_rules! record {
            ($level:expr, $message:expr) => {
                tracing::event!($level, request_id=s.request_id, status=?status,
                    worker_stage=?worker_stage, metrics_final=status.is_some() && s.pending_phases.is_empty(),
                    pending_phases=?s.pending_phases,
                    auth_lock_wait_ms=?measured("auth_wait", s.auth_lock_wait_ms), auth_ms=s.auth_ms,
                    credential_binding_ms=s.credential_binding_ms,
                    queue_wait_ms=?measured("queue", s.queue_wait_ms),
                    prepare_lock_wait_ms=?measured("prepare_wait", s.prepare_lock_wait_ms),
                    prepare_lock_held_ms=?measured("prepare_held", s.prepare_lock_held_ms),
                    sql_ms=?measured("sql", s.sql_ms), cold_ms=s.cold_ms,
                    validation_lock_wait_ms=?measured("validate_wait", s.validation_lock_wait_ms),
                    validation_lock_held_ms=?measured("validate_held", s.validation_lock_held_ms),
                    elapsed_ms=s.elapsed_ms, retry_count=s.retry_count, returned=?s.returned, total=?s.total,
                    vm_steps=?s.vm_steps, sort_operations=?s.sort_operations,
                    fullscan_steps=?s.fullscan_steps, statements=?s.statements,
                    sqlite_profile_ms=?s.sqlite_profile_ms,
                    timeout_stage=?s.timeout_stage, remaining_budget_ms=?s.remaining_budget_ms,
                    worker_stop_delay_ms=?s.worker_stop_delay_ms,
                    pool_rejections=s.pool_rejections, timeouts=s.timeouts, $message);
            }
        }
        if worker_stage.is_some() {
            record!(tracing::Level::WARN, "Library read worker actually stopped");
        } else {
            record!(tracing::Level::INFO, "Library read request completed");
        }
    }
}
