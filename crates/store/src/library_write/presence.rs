//! Presence windows reuse publication deltas and settle each Media once.
use std::collections::HashMap;

use domain::{LibraryId, MediaId};
use rusqlite::Connection;

use super::publish::PublicationCounters;
use super::publish_batch::{PendingChange, reduce_publication_batch};
use super::reduce_incremental::{FileSummaryFacts, read_file_summary_facts, read_file_units};
use crate::StoreError;

#[derive(Default)]
pub(crate) struct PresenceBatch {
    files: Vec<(LibraryId, MediaId, FileSummaryFacts, Vec<(i32, i32)>)>,
    projections: HashMap<LibraryId, HashMap<MediaId, crate::query_projection::ProjectionBefore>>,
}

impl PresenceBatch {
    pub(crate) fn capture(
        &mut self,
        tx: &Connection,
        library_id: LibraryId,
        media_id: MediaId,
        file_id: &str,
    ) -> Result<(), StoreError> {
        let projections = self.projections.entry(library_id).or_default();
        if let std::collections::hash_map::Entry::Vacant(entry) = projections.entry(media_id) {
            entry.insert(crate::query_projection::capture(
                tx,
                crate::query_projection::ProjectionKey {
                    library_id,
                    media_id,
                },
            )?);
        }
        let before = read_file_summary_facts(tx, file_id)?
            .ok_or_else(|| StoreError::Missing(format!("file {file_id}")))?;
        let units = read_file_units(tx, file_id)?;
        self.files.push((library_id, media_id, before, units));
        Ok(())
    }

    pub(crate) fn settle(self, tx: &Connection) -> Result<(), StoreError> {
        let mut changes: HashMap<LibraryId, HashMap<MediaId, Vec<PendingChange>>> = HashMap::new();
        for (library_id, media_id, before, before_units) in self.files {
            let after = read_file_summary_facts(tx, &before.file_id)?;
            let after_units = if after.is_some() {
                read_file_units(tx, &before.file_id)?
            } else {
                vec![]
            };
            let after = after.unwrap_or_else(|| {
                let mut removed = before.clone();
                removed.published = false;
                removed
            });
            changes
                .entry(library_id)
                .or_default()
                .entry(media_id)
                .or_default()
                .push(PendingChange {
                    before: Some(before),
                    before_units,
                    after,
                    after_units,
                });
        }
        for (library_id, projections) in self.projections {
            reduce_publication_batch(
                tx,
                library_id,
                changes.remove(&library_id).unwrap_or_default(),
                projections,
                &mut PublicationCounters::default(),
            )?;
        }
        Ok(())
    }
}
