//! Instance-shared section admission: four upstream calls and four admitted walls.
use std::sync::Arc;

use serde_json::Value;
use tokio::{sync::Semaphore, task::JoinSet, time::Instant};

use super::{DISCOVER_DEADLINE, row, sections::Section};
use crate::{
    blocking_budget::{BlockingBudget, BlockingError},
    catalog::Catalog,
};

const SECTION_CONCURRENCY: usize = 4;
const WALL_CAPACITY: usize = 4;

pub(crate) struct Executor {
    walls: Arc<Semaphore>,
    sections: Arc<BlockingBudget>,
}
impl Default for Executor {
    fn default() -> Self {
        Self {
            walls: Arc::new(Semaphore::new(WALL_CAPACITY)),
            sections: Arc::new(BlockingBudget::new(SECTION_CONCURRENCY)),
        }
    }
}
impl Executor {
    pub(super) async fn run(
        &self,
        catalog: Arc<dyn Catalog>,
        source: Option<String>,
        sections: Vec<Section>,
    ) -> Result<Vec<Value>, BlockingError> {
        let _wall = self
            .walls
            .clone()
            .try_acquire_owned()
            .map_err(|_| BlockingError::Busy)?;
        let deadline = Instant::now() + DISCOVER_DEADLINE;
        // At most four asynchronous section tasks per admitted wall. Waiting
        // consumes no blocking thread, so pending work is bounded by 4*4.
        let mut tasks = JoinSet::new();
        let mut pending = sections.into_iter().enumerate();
        let mut results = Vec::new();
        for (ordinal, section) in pending.by_ref().take(SECTION_CONCURRENCY) {
            self.spawn(
                &mut tasks,
                catalog.clone(),
                source.clone(),
                section,
                ordinal,
                deadline,
            );
        }
        let complete = async {
            while let Some(result) = tasks.join_next().await {
                let (ordinal, value) =
                    result.map_err(|error| BlockingError::Worker(error.to_string()))??;
                results.push((ordinal, value));
                if let Some((ordinal, section)) = pending.next() {
                    self.spawn(
                        &mut tasks,
                        catalog.clone(),
                        source.clone(),
                        section,
                        ordinal,
                        deadline,
                    );
                }
            }
            Ok::<_, BlockingError>(())
        };
        tokio::time::timeout_at(deadline, complete)
            .await
            .map_err(|_| BlockingError::Timeout)??;
        // Only the fixed section table is sorted, never catalog result rows.
        results.sort_unstable_by_key(|(ordinal, _)| *ordinal);
        Ok(results.into_iter().map(|(_, value)| value).collect())
    }

    fn spawn(
        &self,
        tasks: &mut JoinSet<Result<(usize, Value), BlockingError>>,
        catalog: Arc<dyn Catalog>,
        source: Option<String>,
        section: Section,
        ordinal: usize,
        deadline: Instant,
    ) {
        let budget = self.sections.clone();
        tasks.spawn(async move {
            let reservation = budget
                .reserve(deadline.saturating_duration_since(Instant::now()))
                .await?;
            // The reservation lives in the actual blocking closure. A wall
            // timeout aborts observers, but running calls retain capacity until
            // they really finish; no nested http_work permit is acquired.
            reservation
                .run_to_completion(move || {
                    let selected = match source.as_deref() {
                        Some(source) => catalog.source(source).unwrap_or(catalog.as_ref()),
                        None => catalog.as_ref(),
                    };
                    let value = row(
                        selected.source_name(),
                        section.kind,
                        section.id,
                        section.title,
                        section.presentation,
                        (section.fetch)(selected),
                    );
                    (ordinal, value)
                })
                .await
        });
    }
}
