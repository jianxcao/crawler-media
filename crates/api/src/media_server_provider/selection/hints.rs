//! Bounded hint references share the standard page fact/hydration boundary.
use super::*;
use crate::library_read_work::run_store_query_stage;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

pub(in crate::media_server_provider) async fn hints(
    provider: &ApiServerProvider,
    user: UserId,
    term: &str,
    offset: usize,
    limit: usize,
) -> Result<ServerItemPage, String> {
    let deadline = Instant::now() + SELECTION_DEADLINE;
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
        })
    })
    .await
    .map_err(|error| error.to_string())?;
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
    .map_err(|error| error.to_string())?;
    if let store::PageValidation::Stale { gone } = validation {
        let gone: HashSet<_> = gone.into_iter().collect();
        page.items.retain(|item| !gone.contains(&item.file.file_id));
    }
    Ok(page)
}
fn attempt_error(error: AttemptError) -> String {
    match error {
        AttemptError::Query(error) => error.to_string(),
        AttemptError::Changed | AttemptError::SourcesChanged => {
            "Library facts changed during search hints".into()
        }
    }
}
