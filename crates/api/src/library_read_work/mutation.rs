//! Deadline-aware mutations use the common writer admission, never a pooled reader.
use super::*;

pub(crate) async fn run_store_write_stage_named<T: Send + 'static>(
    state: &ApiState,
    deadline: Instant,
    stage: &'static str,
    work: impl FnOnce(&Store) -> Result<T, LibraryReadError> + Send + 'static,
) -> Result<T, LibraryReadError> {
    let phase = StageMetrics::start(stage);
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel_on_drop = CancelOnDrop(cancelled.clone());
    let budget = QueryBudget {
        deadline,
        cancelled,
    };
    budget.check()?;
    let wait = deadline.saturating_duration_since(Instant::now());
    let mutation = crate::http::store_wait::store_write(state, move |store| {
        budget.check()?;
        store.with_query_budget(&budget, work)
    });
    match tokio::time::timeout(wait, mutation).await {
        Ok(Ok(result)) => result,
        Ok(Err(response)) => {
            phase.rejected();
            tracing::warn!(status = %response.status(), stage, "Library writer admission rejected");
            Err(LibraryReadError::Busy)
        }
        Err(_) => {
            phase.timeout();
            Err(LibraryReadError::Deadline)
        }
    }
}
