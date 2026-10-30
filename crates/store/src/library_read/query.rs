use std::collections::{HashMap, HashSet};

use domain::{MediaId, MediaKind, ProtocolItemPage, TypedItemId};
use rusqlite::Connection;

use super::{LibraryReadContext, LibraryReadError, LibraryReadRequest, QueryBudget, ReaderLease};
use crate::library_query::{
    facets::library_facets_on,
    items::{
        hydration::{library_item_facts_on, library_item_stills_on},
        page::{library_item_index_on, library_item_page_on},
    },
    page_inputs::{ProtocolPageInputs, page_inputs_on},
    protocol::select_protocol,
    series_scalar::{ProtocolSeriesScalar, series_scalar_on},
    types::{ItemPage, LibraryFacets, LibraryItemFacts, LibraryStill},
};

pub enum LibraryReadResult {
    ItemIndex(Vec<crate::library_query::IndexBucket>),
    Browse {
        page: ItemPage,
        facts: HashMap<MediaId, LibraryItemFacts>,
        stills: Vec<LibraryStill>,
    },
    Facets(LibraryFacets),
    Protocol {
        page: ProtocolItemPage,
        inputs: ProtocolPageInputs,
        series: HashMap<MediaId, ProtocolSeriesScalar>,
    },
}

impl ReaderLease {
    /// Diagnostic-only complete leased SQL scope: initial layout, BEGIN, roster,
    /// selection/hydration, ROLLBACK and final layout. Normal execute has no tracer.
    /// `rows_returned` and `hydrated_rows` are unfilled; their default zero is not
    /// a measurement. Callers measure final response counts separately. This scope
    /// excludes checkout/connection-open and short Store/cold/validation SQL.
    pub fn execute_observed(
        &mut self,
        context: &LibraryReadContext,
        budget: &QueryBudget,
    ) -> (
        Result<LibraryReadResult, LibraryReadError>,
        crate::library_query::items::QueryWorkStats,
    ) {
        let mut stats = crate::library_query::items::QueryWorkStats::default();
        let result = self
            .with_connection_instrumented(
                budget,
                |conn| Ok(execute_on(conn, context)),
                Some(&mut stats),
                true,
            )
            .and_then(|owned| owned);
        (result, stats)
    }

    pub fn execute(
        &mut self,
        context: &LibraryReadContext,
        budget: &QueryBudget,
    ) -> Result<LibraryReadResult, LibraryReadError> {
        // The B1 callback is StoreError-only. Nest the typed query outcome inside its
        // successful owned value; context and authorization never travel as SQL text.
        let result = self
            .with_connection_instrumented(budget, |conn| Ok(execute_on(conn, context)), None, true)
            .and_then(|owned| owned);
        if let Err(error) = &result {
            super::context::log_rejection(
                "execute",
                context.user(),
                context.request.explicit_library(),
                error,
            );
        }
        result
    }
}

fn execute_on(
    conn: &Connection,
    context: &LibraryReadContext,
) -> Result<LibraryReadResult, LibraryReadError> {
    super::context::verify_roster(conn, context)?;
    match &context.request {
        LibraryReadRequest::Browse(request) => {
            if !context.has_library(request.library_id) {
                return Err(LibraryReadError::Unauthorized);
            }
            let (page, _) = library_item_page_on(conn, request)?;
            let ids: Vec<_> = page.items.iter().map(|item| item.media_id).collect();
            let facts = library_item_facts_on(conn, request.library_id, &ids, request.user_id)?;
            let stills = if request.filter.require_image {
                library_item_stills_on(conn, request.library_id, &ids)?
            } else {
                Vec::new()
            };
            Ok(LibraryReadResult::Browse {
                page,
                facts,
                stills,
            })
        }
        LibraryReadRequest::ItemIndex(request) => {
            if !context.has_library(request.library_id) {
                return Err(LibraryReadError::Unauthorized);
            }
            Ok(LibraryReadResult::ItemIndex(library_item_index_on(
                conn, request,
            )?))
        }
        LibraryReadRequest::Facets(request) => {
            if !context.has_library(request.library_id) {
                return Err(LibraryReadError::Unauthorized);
            }
            Ok(LibraryReadResult::Facets(library_facets_on(conn, request)?))
        }
        LibraryReadRequest::Protocol(request) => protocol_on(conn, context, request),
    }
}

fn protocol_on(
    conn: &Connection,
    context: &LibraryReadContext,
    request: &domain::ProtocolItemQuery,
) -> Result<LibraryReadResult, LibraryReadError> {
    let allowed = context.authorized();
    let page = select_protocol(conn, &allowed, request)?;
    let inputs = page_inputs_on(
        conn,
        &allowed,
        context.user(),
        context.is_admin(),
        &page.items,
    )?;
    let allowed_strings: HashSet<_> = allowed.iter().map(ToString::to_string).collect();
    let mut series = HashMap::new();
    for reference in &page.items {
        if let TypedItemId::Work(media) = reference.item {
            if !inputs
                .media
                .get(&media)
                .is_some_and(|row| row.kind == MediaKind::Tv)
                || series.contains_key(&media)
            {
                continue;
            }
            let scalar = series_scalar_on(
                conn,
                &allowed_strings,
                context.user(),
                media,
                &request.parent,
            )?;
            series.insert(media, scalar);
        }
    }
    Ok(LibraryReadResult::Protocol {
        page,
        inputs,
        series,
    })
}
