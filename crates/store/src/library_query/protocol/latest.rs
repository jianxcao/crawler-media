//! Count-free Latest selection, with only selected references hydrated by the caller.
use std::collections::HashSet;

use domain::{LibraryId, ProtocolItemPage, ProtocolItemQuery};
use rusqlite::Connection;

use crate::StoreError;

use super::select_protocol_inner;

pub(crate) fn select_latest(
    tx: &Connection,
    authorized: &HashSet<LibraryId>,
    request: &ProtocolItemQuery,
) -> Result<ProtocolItemPage, StoreError> {
    if request.limit == 0 {
        return Ok(ProtocolItemPage {
            items: Vec::new(),
            total: 0,
        });
    }
    let mut segment = request.clone();
    if request.is_played.is_some() {
        segment.limit = request.limit.saturating_add(1);
        let mut page = select_protocol_inner(tx, authorized, &segment, false)?;
        page.items.truncate(request.limit);
        page.total = page.items.len() as u64;
        return Ok(page);
    }
    // Prefix references, not metadata: exhaustion proves the segment boundary.
    // Both queries use this same connection and therefore the same WAL snapshot.
    segment.is_played = Some(false);
    segment.offset = 0;
    segment.limit = request
        .offset
        .saturating_add(request.limit)
        .saturating_add(1);
    let prefix = select_protocol_inner(tx, authorized, &segment, false)?;
    let unplayed = prefix.items.len();
    let mut items: Vec<_> = prefix
        .items
        .into_iter()
        .skip(request.offset)
        .take(request.limit)
        .collect();
    if items.len() < request.limit {
        segment.is_played = Some(true);
        segment.offset = request.offset.saturating_sub(unplayed);
        segment.limit = request.limit.saturating_sub(items.len()).saturating_add(1);
        let played = select_protocol_inner(tx, authorized, &segment, false)?;
        items.extend(played.items.into_iter().take(request.limit - items.len()));
    }
    Ok(ProtocolItemPage {
        total: items.len() as u64,
        items,
    })
}
