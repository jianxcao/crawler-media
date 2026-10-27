//! One projection settlement per dirty Media in a publication window.
use super::reduce_incremental::{
    BatchSettled, ChangedFile, FileSummaryFacts, reduce_library_media_incremental_in_batch,
};
use crate::StoreError;
use domain::{LibraryId, MediaId};
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
/// The per-media outcome of a publication batch: a single changed file can be reduced
/// incrementally, anything else needs the full recompute.
/// Whether `(library, media)` already has a card row to move by deltas.
pub(super) fn item_card_exists(
    tx: &Connection,
    library_id: LibraryId,
    media_id: MediaId,
) -> Result<bool, StoreError> {
    let found: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM library_items WHERE library_id = ?1 AND media_id = ?2",
            rusqlite::params![library_id.to_string(), media_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// One published file's before/after facts, kept so the reduction can apply per-file
/// deltas instead of re-reading every published file of the media once per window.
pub(super) struct PendingChange {
    pub(super) before: Option<FileSummaryFacts>,
    pub(super) before_units: Vec<(i32, i32)>,
    pub(super) after: FileSummaryFacts,
    pub(super) after_units: Vec<(i32, i32)>,
}

/// All file facts have been written; reduce each card and publish its protocol
/// index/user summaries once, before this publication transaction commits.
pub(super) fn reduce_publication_batch(
    tx: &Connection,
    library_id: LibraryId,
    affected: HashMap<MediaId, Vec<PendingChange>>,
    mut before: HashMap<MediaId, crate::query_projection::ProjectionBefore>,
    counters: &mut super::publish::PublicationCounters,
) -> Result<(), StoreError> {
    let mut affected = affected.into_iter().collect::<Vec<_>>();
    affected.sort_by_key(|(id, _)| id.to_string());
    for (media_id, changes) in affected {
        counters.media_reduced();
        let key = crate::query_projection::ProjectionKey {
            library_id,
            media_id,
        };
        let captured = before
            .remove(&media_id)
            .ok_or_else(|| StoreError::Invalid("missing publication projection capture".into()))?;
        // A media without a card yet cannot be moved by deltas: the incremental path
        // repairs it with a full reduce, and applying the remaining deltas afterwards
        // would count those files twice. Do the full reduce once and skip the rest.
        if !item_card_exists(tx, library_id, media_id)? {
            super::reduce::reduce_library_media_facts(tx, library_id, media_id)?;
            crate::query_projection::settle(tx, key, captured)?;
            continue;
        }
        // Apply one delta per changed file. A media that appears in several windows of a
        // scan used to fall back to a full `reduce_library_media` per window, which
        // re-reads every published file of that media (O(files x windows)). Per-file
        // deltas keep each window proportional to the files it touched; totals match
        // because deltas apply in publication order.
        //
        // Every file of the batch is already written, so a unit (or card) that has
        // to be rebuilt from facts already counts the later files of the batch:
        // `settled` keeps those rows from receiving the same files again.
        let mut settled = BatchSettled {
            defer_user_summaries: true,
            ..BatchSettled::default()
        };
        for change in changes {
            reduce_library_media_incremental_in_batch(
                tx,
                library_id,
                media_id,
                &ChangedFile {
                    before: change.before.as_ref(),
                    before_units: &change.before_units,
                    after: &change.after,
                    after_units: &change.after_units,
                },
                &mut settled,
            )?;
        }
        if settled.user_summaries_dirty && !settled.media {
            super::reduce::refresh_affected_user_summaries(tx, library_id, media_id)?;
        }
        if settled.protocol_index_dirty && !settled.media {
            crate::protocol_index::sync_card(tx, &library_id.to_string(), &media_id.to_string())?;
        }
        crate::query_projection::settle(tx, key, captured)?;
    }
    Ok(())
}
