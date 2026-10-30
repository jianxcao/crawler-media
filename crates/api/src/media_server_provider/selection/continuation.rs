//! Continuation references are selected before page facts and cold hydration.
use super::*;
use crate::library_read_work::{run_store_query_stage, run_store_query_stage_named};
use domain::{ProtocolItemPage, ProtocolItemRef};
use std::{collections::HashSet, sync::atomic::AtomicBool, time::Instant};

#[derive(Clone, Copy)]
enum Kind {
    NextUp,
    Resume,
}

pub(in crate::media_server_provider) async fn next_up_items(
    provider: &ApiServerProvider,
    user: UserId,
    series: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<ServerItemPage, String> {
    continuation(
        &provider.state,
        user,
        ServerItemQuery {
            series_id: series.map(str::to_owned),
            offset,
            limit,
            recursive: true,
            ..Default::default()
        },
        Kind::NextUp,
    )
    .await
}

pub(in crate::media_server_provider) async fn resumable_items(
    provider: &ApiServerProvider,
    user: UserId,
    parent: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<ServerItemPage, String> {
    continuation(
        &provider.state,
        user,
        ServerItemQuery {
            parent_id: parent.map(str::to_owned),
            offset,
            limit,
            recursive: true,
            ..Default::default()
        },
        Kind::Resume,
    )
    .await
}

async fn continuation(
    state: &ApiState,
    user: UserId,
    query: ServerItemQuery,
    kind: Kind,
) -> Result<ServerItemPage, String> {
    let deadline = Instant::now() + SELECTION_DEADLINE;
    let mut admission = Admission {
        budget: store::QueryBudget {
            deadline,
            cancelled: Arc::new(AtomicBool::new(false)),
        },
        complete: false,
    };
    for attempt in 0..2 {
        let request = query.clone();
        let result = run_store_query_stage(state, deadline, move |store| {
            capture(store, user, &request, kind)
        })
        .await
        .map_err(|error| error.to_string())?;
        let (mut page, references, snapshot) = match finish_protocol(
            state,
            deadline,
            user,
            query.clone(),
            result,
            admission.budget.clone(),
        )
        .await
        {
            Ok(result) => result,
            Err(AttemptError::Changed | AttemptError::SourcesChanged) if attempt == 0 => continue,
            Err(error) => return Err(attempt_error(error)),
        };
        state
            .library_reads
            .check_open()
            .map_err(|error| error.to_string())?;
        let validation = run_store_query_stage_named(state, deadline, "validate", move |store| {
            validate(store, user, &references, &snapshot)
        })
        .await
        .map_err(|error| error.to_string())?;
        if let store::PageValidation::Stale { gone } = validation {
            if attempt == 0 {
                continue;
            }
            let gone: HashSet<_> = gone.into_iter().collect();
            page.items.retain(|item| !gone.contains(&item.file.file_id));
        }
        admission.complete = true;
        media_server::query_metrics::QueryMetrics::page(
            page.items.len() as u64,
            Some(page.total as u64),
        );
        return Ok(page);
    }
    Err("Library continuation changed during selection".into())
}

fn capture(
    store: &crate::Store,
    user: UserId,
    query: &ServerItemQuery,
    kind: Kind,
) -> Result<store::LibraryReadResult, store::LibraryReadError> {
    let mut scoped_query = query.clone();
    if let Some(parent) = query.parent_id.as_deref()
        && let Some(domain::TypedItemId::Season { media, .. }) =
            domain::TypedItemId::from_wire_string(parent)
    {
        scoped_query.parent_id = Some(domain::TypedItemId::Work(media).to_wire_string());
        scoped_query.season_id = Some(parent.to_owned());
    }
    let resolved = super::request::resolve_page_request(store, user, &scoped_query)
        .map_err(|error| store::StoreError::Invalid(error.to_string()))?;
    let selected = if let Some(resolved) = resolved {
        let libraries = store
            .authorized_library_ids(user)?
            .into_iter()
            .map(|id| id.parse())
            .collect::<Result<Vec<_>, _>>()
            .map_err(store::StoreError::from)?;
        let mut request = store::ContinueQuery {
            user_id: user,
            libraries,
            parent: resolved.parent,
            media_id: resolved.series_id,
            season: resolved.season.map(|(_, season)| season as i32),
            limit: query.limit,
            offset: query.offset,
        };
        let selected = select(store, &request, kind)?;
        if selected.total == 0
            && matches!(request.parent, domain::ProtocolParent::Genre(_))
            && query.parent_id.is_some()
        {
            request.parent =
                domain::ProtocolParent::Studio(query.parent_id.clone().unwrap_or_default());
            select(store, &request, kind)?
        } else {
            selected
        }
    } else {
        store::ContinuePage::default()
    };
    let page = ProtocolItemPage {
        items: selected
            .items
            .into_iter()
            .map(|row| row.reference)
            .collect(),
        total: selected.total as u64,
    };
    let inputs = store.protocol_page_inputs(user, &page.items)?;
    Ok(store::LibraryReadResult::Protocol {
        page,
        inputs,
        series: HashMap::new(),
    })
}

fn select(
    store: &crate::Store,
    request: &store::ContinueQuery,
    kind: Kind,
) -> Result<store::ContinuePage, store::StoreError> {
    match kind {
        Kind::NextUp => store.next_up_page(request),
        Kind::Resume => store.resume_page(request),
    }
}

fn validate(
    store: &crate::Store,
    user: UserId,
    references: &[ProtocolItemRef],
    snapshot: &store::PageSnapshot,
) -> Result<store::PageValidation, store::LibraryReadError> {
    let allowed: HashSet<String> = store.authorized_library_ids(user)?.into_iter().collect();
    let mut gone: HashSet<_> = references
        .iter()
        .filter(|row| !allowed.contains(&row.library_id.to_string()))
        .filter_map(|row| row.preferred_source.as_ref().map(|source| source.file_id))
        .collect();
    match store.validate_page_snapshot(snapshot)? {
        store::PageValidation::Fresh if gone.is_empty() => Ok(store::PageValidation::Fresh),
        store::PageValidation::Fresh => Ok(store::PageValidation::Stale {
            gone: gone.into_iter().collect(),
        }),
        store::PageValidation::Stale { gone: changed } => {
            gone.extend(changed);
            Ok(store::PageValidation::Stale {
                gone: gone.into_iter().collect(),
            })
        }
    }
}

fn attempt_error(error: AttemptError) -> String {
    match error {
        AttemptError::Query(error) => error.to_string(),
        AttemptError::Changed | AttemptError::SourcesChanged => {
            "Library continuation changed during hydration".into()
        }
    }
}
struct Admission {
    budget: store::QueryBudget,
    complete: bool,
}
impl Drop for Admission {
    fn drop(&mut self) {
        if !self.complete {
            self.budget.cancelled.store(true, Ordering::Release);
        }
    }
}
