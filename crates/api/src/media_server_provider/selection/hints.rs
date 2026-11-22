//! Bounded hint references share the standard page fact/hydration boundary.
use super::*;
use crate::library_read_work::run_store_query_stage;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use media_server::provider::QueryError;

pub(in crate::media_server_provider) async fn hints(
    provider: &ApiServerProvider,
    user: UserId,
    term: &str,
    offset: usize,
    limit: usize,
) -> Result<ServerItemPage, QueryError> {
    let deadline = Instant::now() + SELECTION_DEADLINE;
    for attempt in 0..2 {
        let observed_access = run_store_query_stage(&provider.state, deadline, move |store| {
            Ok(store.authorized_library_ids(user)?)
        })
        .await
        .map_err(super::super::query_stage_error)?;
        let page = hints_attempt(provider, user, term, offset, limit, deadline).await?;
        let current_access = run_store_query_stage(&provider.state, deadline, move |store| {
            Ok(store.authorized_library_ids(user)?)
        })
        .await
        .map_err(super::super::query_stage_error)?;
        if current_access == observed_access {
            return Ok(page);
        }
        if attempt == 0 {
            tracing::warn!(%user, "Jellyfin Library access changed during search hints; retrying selection");
        }
    }
    Err(QueryError::Unavailable)
}

async fn hints_attempt(
    provider: &ApiServerProvider,
    user: UserId,
    term: &str,
    offset: usize,
    limit: usize,
    deadline: Instant,
) -> Result<ServerItemPage, QueryError> {
    let term = term.to_owned();
    let result = run_store_query_stage(&provider.state, deadline, move |store| {
        let page = store.protocol_search_hints(user, &term, offset, limit)?;
        let inputs = store.protocol_page_inputs(user, &page.items)?;
        let mut series = HashMap::new();
        for reference in &page.items {
            let media = reference.item.media_id();
            if matches!(reference.item, domain::TypedItemId::Work(_))
                && inputs
                    .media
                    .get(&media)
                    .is_some_and(|row| row.kind == MediaKind::Tv)
            {
                series.insert(
                    media,
                    store.protocol_series_scalar(user, media, &domain::ProtocolParent::Root)?,
                );
            }
        }
        Ok(store::LibraryReadResult::Protocol {
            page,
            inputs,
            series,
            hydration: None,
        })
    })
    .await
    .map_err(super::super::query_stage_error)?;
    let budget = store::QueryBudget {
        deadline,
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let (mut page, _, snapshot) = finish_protocol(
        &provider.state,
        deadline,
        user,
        ServerItemQuery {
            offset,
            limit,
            ..Default::default()
        },
        result,
        budget,
    )
    .await
    .map_err(attempt_error)?;
    let validation = run_store_query_stage(&provider.state, deadline, move |store| {
        Ok(store.validate_page_snapshot(&snapshot)?)
    })
    .await
    .map_err(super::super::query_stage_error)?;
    if let store::PageValidation::Stale { gone } = validation {
        let gone: HashSet<_> = gone.into_iter().collect();
        page.items.retain(|item| !gone.contains(&item.file.file_id));
    }
    Ok(page)
}
fn attempt_error(error: AttemptError) -> QueryError {
    match error {
        AttemptError::Query(error) => error,
        AttemptError::Changed | AttemptError::SourcesChanged => QueryError::Unavailable,
    }
}
