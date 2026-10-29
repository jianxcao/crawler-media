//! One publication boundary for a protocol page or complete Latest response.
use super::super::ApiServerProvider;
use crate::{
    ApiState,
    library_read_work::{run_lease_stage, run_store_query_stage, run_store_query_stage_named},
};
use domain::{LibraryId, ProtocolItemRef, ProtocolParent};
use media_server::provider::{
    MediaItemSnapshot, QueryError, QueryPrincipal, ServerItemPage, ServerItemQuery,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;
use store::{
    LibraryReadContext, LibraryReadError, LibraryReadRequest, LibraryReadResult, ReadCredential,
};

#[derive(Clone, Copy)]
enum ResponseKind {
    Page,
    Latest,
}

pub(in crate::media_server_provider) async fn query_visible_items(
    provider: &ApiServerProvider,
    principal: QueryPrincipal,
    query: ServerItemQuery,
) -> Result<ServerItemPage, QueryError> {
    query_response(&provider.state, principal, query, ResponseKind::Page).await
}

pub(in crate::media_server_provider) async fn query_latest_items(
    provider: &ApiServerProvider,
    principal: QueryPrincipal,
    query: ServerItemQuery,
) -> Result<Vec<MediaItemSnapshot>, QueryError> {
    Ok(
        query_response(&provider.state, principal, query, ResponseKind::Latest)
            .await?
            .items,
    )
}

async fn query_response(
    state: &ApiState,
    principal: QueryPrincipal,
    query: ServerItemQuery,
    kind: ResponseKind,
) -> Result<ServerItemPage, QueryError> {
    // Once the request resolves a Library, retries must preserve its identity.
    let mut explicit_library = None;
    for attempt in 0..2 {
        let mut admission = ProbeAdmission::new(principal.deadline);
        let mut work = Attempt {
            probe_budget: admission.budget.clone(),
            state,
            principal: &principal,
            explicit_library: &mut explicit_library,
            contexts: Vec::new(),
            references: Vec::new(),
            latest_total: None,
            snapshot: None,
        };
        let result = async {
            let mut page = match kind {
                ResponseKind::Page => work.segment(query.clone()).await?,
                ResponseKind::Latest => work.latest(&query).await?,
            };
            let total = match kind {
                ResponseKind::Page => Some(page.total as u64),
                ResponseKind::Latest => work.latest_total,
            };
            if let store::PageValidation::Stale { gone } = work.validate().await? {
                if attempt == 0 {
                    return Err(AttemptError::SourcesChanged);
                }
                let gone = gone.into_iter().collect::<std::collections::HashSet<_>>();
                page.items.retain(|item| !gone.contains(&item.file.file_id));
                page.total = page.total.max(page.items.len());
            }
            Ok((page, total))
        }
        .await;
        match result {
            Ok((page, total)) => {
                admission.complete();
                media_server::query_metrics::QueryMetrics::page(page.items.len() as u64, total);
                return Ok(page);
            }
            Err(AttemptError::Changed | AttemptError::SourcesChanged) if attempt == 0 => {
                media_server::query_metrics::QueryMetrics::retry();
                tracing::warn!(user_id = %principal.user_id, "Jellyfin Library context changed; retrying complete response");
            }
            Err(AttemptError::Changed | AttemptError::SourcesChanged) => {
                return Err(QueryError::Unavailable);
            }
            Err(AttemptError::Query(error)) => return Err(error),
        }
    }
    Err(QueryError::Unavailable)
}

/// Admission may outlive successful cold rendering, but remains owned by the
/// whole response attempt until publication validation succeeds.
struct ProbeAdmission {
    budget: store::QueryBudget,
    cancel_on_drop: bool,
}
impl ProbeAdmission {
    fn new(deadline: Instant) -> Self {
        Self {
            budget: store::QueryBudget {
                deadline,
                cancelled: Arc::new(AtomicBool::new(false)),
            },
            cancel_on_drop: true,
        }
    }
    fn complete(&mut self) {
        self.cancel_on_drop = false;
    }
}
impl Drop for ProbeAdmission {
    fn drop(&mut self) {
        if self.cancel_on_drop {
            self.budget.cancelled.store(true, Ordering::Release);
        }
    }
}

pub(super) enum AttemptError {
    Changed,
    SourcesChanged,
    Query(QueryError),
}
impl From<LibraryReadError> for AttemptError {
    fn from(error: LibraryReadError) -> Self {
        if matches!(error, LibraryReadError::ContextChanged) {
            Self::Changed
        } else {
            Self::Query(super::super::query_stage_error(error))
        }
    }
}
impl From<QueryError> for AttemptError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

struct Attempt<'a> {
    probe_budget: store::QueryBudget,
    state: &'a ApiState,
    principal: &'a QueryPrincipal,
    explicit_library: &'a mut Option<LibraryId>,
    contexts: Vec<LibraryReadContext>,
    references: Vec<ProtocolItemRef>,
    latest_total: Option<u64>,
    snapshot: Option<store::PageSnapshot>,
}
impl Attempt<'_> {
    async fn latest(&mut self, query: &ServerItemQuery) -> Result<ServerItemPage, AttemptError> {
        let mut segment = query.clone();
        segment.sort_by = Some("DateCreated,SortName".into());
        segment.sort_order = Some("Descending".into());
        if query.is_played.is_some() {
            let page = self.segment(segment).await?;
            self.latest_total = Some(page.total as u64);
            return Ok(page);
        }
        segment.is_played = Some(false);
        let mut unplayed = self.segment(segment.clone()).await?;
        if unplayed.items.len() < query.limit {
            let taken = unplayed.items.len();
            segment.is_played = Some(true);
            segment.offset = (query.offset + taken).saturating_sub(unplayed.total);
            segment.limit = query.limit - taken;
            let played = self.segment(segment).await?;
            self.latest_total = Some(unplayed.total as u64 + played.total as u64);
            unplayed.items.extend(played.items);
        }
        // An unselected played segment has no measured total. Latest's array
        // wire response has no total; do not add a query just for instrumentation.
        Ok(unplayed)
    }

    async fn segment(&mut self, query: ServerItemQuery) -> Result<ServerItemPage, AttemptError> {
        let user_id = self.principal.user_id;
        let deadline = self.principal.deadline;
        let credential = ReadCredential::new(user_id, self.principal.token.clone());
        let resolved_query = query.clone();
        let timing_state = self.state.clone();
        let roster_credential = credential.clone();
        let known_library = *self.explicit_library;
        let (scope_context, resolved) = run_store_query_stage(self.state, deadline, move |store| {
            let context = store
                .prepare_library_read(&roster_credential, &LibraryReadRequest::roster(user_id))?;
            require_explicit(&context, known_library)?;
            let started = std::time::Instant::now();
            let request = super::request::resolve_page_request(store, user_id, &resolved_query);
            timing_state.record_query_phase("selection", started.elapsed());
            Ok((context, request))
        })
        .await?;
        self.contexts.push(scope_context);
        let Some(request) = resolved? else {
            return Ok(ServerItemPage::default());
        };
        if let ProtocolParent::Library(id) = request.parent {
            *self.explicit_library = Some(id);
        }
        let mut result = self.lease(&credential, request.clone()).await?;
        if matches!(&result, LibraryReadResult::Protocol { page, .. } if page.total == 0)
            && matches!(request.parent, ProtocolParent::Genre(_))
            && query.parent_id.is_some()
        {
            let mut fallback = request;
            fallback.parent = ProtocolParent::Studio(query.parent_id.clone().unwrap_or_default());
            result = self.lease(&credential, fallback).await?;
        }
        let (page, references, snapshot) = super::finish_protocol(
            self.state,
            deadline,
            user_id,
            query,
            result,
            self.probe_budget.clone(),
        )
        .await?;
        self.references.extend(references);
        if let Some(combined) = &mut self.snapshot {
            combined.files.extend(snapshot.files);
        } else {
            self.snapshot = Some(snapshot);
        }
        Ok(page)
    }

    async fn lease(
        &mut self,
        credential: &ReadCredential,
        request: domain::ProtocolItemQuery,
    ) -> Result<LibraryReadResult, AttemptError> {
        let credential = credential.clone();
        let context_request = LibraryReadRequest::Protocol(request);
        let known_library = *self.explicit_library;
        let context = run_store_query_stage(self.state, self.principal.deadline, move |store| {
            let context = store.prepare_library_read(&credential, &context_request)?;
            require_explicit(&context, known_library)?;
            Ok(context)
        })
        .await?;
        let (context, result) =
            run_lease_stage(self.state, context, self.principal.deadline).await?;
        self.contexts.push(context);
        Ok(result)
    }

    async fn validate(self) -> Result<store::PageValidation, AttemptError> {
        self.state.library_reads.check_open()?;
        let validation = run_store_query_stage_named(
            self.state,
            self.principal.deadline,
            "validate",
            move |store| {
                if let Some(snapshot) = self.snapshot {
                    store.validate_protocol_page(&self.contexts, &self.references, &snapshot)
                } else {
                    for context in self.contexts {
                        store.validate_library_read(&context)?;
                    }
                    Ok(store::PageValidation::Fresh)
                }
            },
        )
        .await?;
        self.state.library_reads.check_open()?;
        Ok(validation)
    }
}

fn require_explicit(
    context: &LibraryReadContext,
    library: Option<LibraryId>,
) -> Result<(), LibraryReadError> {
    if library.is_some_and(|id| !context.has_library(id)) {
        tracing::error!(?library, "Jellyfin explicitly resolved Library access lost");
        Err(LibraryReadError::Unauthorized)
    } else {
        Ok(())
    }
}
