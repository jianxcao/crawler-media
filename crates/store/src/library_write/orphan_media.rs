//! Purge Media that no longer back any Library file.
//!
//! A Media row exists to describe Library content. Once its last
//! `library_files` row is gone and nothing user-curated still points at it
//! (a collection entry or a subscription), the Media and every row it owns —
//! aliases, metadata, episodes, markers, probe jobs, fingerprint models and
//! per-user playback state — are removed in the caller's transaction.
//! Historic logs (`playback_logs`, `playback_metrics`, removal receipts) are
//! history, not ownership, and stay.

use std::collections::HashSet;

use domain::MediaId;
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;

/// Child tables keyed by `media_id`, deleted before `media` (FK order).
const OWNED: &[&str] = &[
    "library_units",
    "library_items",
    "library_metadata_inputs",
    "playback_session_ids",
    "playback_sessions",
    "playback_units",
    "probe_season_comparison_history",
    "fingerprint_season_models",
    "media_markers",
    "media_episodes",
    "media_metadata_overrides",
    "media_metadata_sources",
    "media_metadata",
    "media_aliases",
];

impl crate::Store {}

/// Purge every candidate Media that has no file, no collection entry, no
/// subscription (`subscribed`) and is not a merge survivor. Returns the purged ids.
pub(crate) fn purge_orphan_media_in_tx(
    tx: &Connection,
    candidates: &HashSet<MediaId>,
) -> Result<Vec<MediaId>, StoreError> {
    let mut purged = Vec::new();
    for media in candidates {
        let id = media.to_string();
        let frozen: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM media WHERE media_id=?1 AND identity_state='merging')",
            [&id],
            |r| r.get(0),
        )?;
        if frozen {
            tx.execute(
                "INSERT OR IGNORE INTO pending_orphan_media VALUES(?1)",
                [&id],
            )?;
            continue;
        }
        let referenced: Option<i64> = tx
            .query_row(
                "SELECT 1 WHERE EXISTS(SELECT 1 FROM library_files WHERE media_id=?1)
                    OR EXISTS(SELECT 1 FROM collection_items WHERE media_id=?1)
                    OR EXISTS(SELECT 1 FROM subscribe_retention WHERE media_id=?1)
                    OR EXISTS(SELECT 1 FROM media WHERE survivor_id=?1)
                    OR EXISTS(SELECT 1 FROM media WHERE media_id=?1 AND identity_state='retired')
                    OR NOT EXISTS(SELECT 1 FROM media WHERE media_id=?1)",
                [&id],
                |row| row.get(0),
            )
            .optional()?;
        if referenced.is_some() {
            continue;
        }
        tx.execute(
            "DELETE FROM probe_job_units WHERE job_id IN (SELECT id FROM probe_jobs WHERE media_id=?1)",
            [&id],
        )?;
        tx.execute("DELETE FROM probe_jobs WHERE media_id=?1", [&id])?;
        crate::query_projection::search::remove_media(tx, &id)?;
        for table in OWNED {
            tx.execute(
                &format!("DELETE FROM {table} WHERE media_id=?1"),
                params![id],
            )?;
        }
        tx.execute("DELETE FROM media WHERE media_id=?1", [&id])?;
        tracing::info!(media_id = %id, "Purged Media without Library files");
        purged.push(*media);
    }
    Ok(purged)
}

impl crate::Store {
    pub fn recover_orphan_media(&self) -> Result<usize, StoreError> {
        let ids=self.with_writer(crate::Db::Library,|conn| {
            let tx=crate::write_tx(conn)?;
            let mut after:String=tx.query_row("SELECT cursor FROM reference_recovery_cursors WHERE kind='orphan'",[],|r|r.get(0))?;
            let more:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM pending_orphan_media p JOIN media m USING(media_id) WHERE m.identity_state<>'merging' AND p.media_id>?1)",[&after],|r|r.get(0))?;
            if !more {after.clear();}
            let mut stmt=tx.prepare("SELECT p.media_id FROM pending_orphan_media p JOIN media m USING(media_id) WHERE m.identity_state<>'merging' AND p.media_id>?1 ORDER BY p.media_id LIMIT 64")?;
            let ids=stmt.query_map([after],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            drop(stmt);
            tx.execute("UPDATE reference_recovery_cursors SET cursor=?1 WHERE kind='orphan'",[ids.last().map_or("",String::as_str)])?;
            tx.commit()?;Ok(ids)
        })?;
        let mut failure = None;
        for raw_id in &ids {
            let result = raw_id
                .parse::<MediaId>()
                .map_err(StoreError::from)
                .and_then(|id| {
                    self.with_writer(crate::Db::Library, |conn| {
                        let tx = crate::write_tx(conn)?;
                        // Candidate removal and purge commit together, independently
                        // of any other poisoned candidate in the selected window.
                        tx.execute(
                            "DELETE FROM pending_orphan_media WHERE media_id=?1",
                            [raw_id],
                        )?;
                        purge_orphan_media_in_tx(&tx, &HashSet::from([id]))?;
                        tx.commit()?;
                        Ok(())
                    })
                });
            if let Err(error) = result {
                tracing::error!(media_id=%raw_id,%error,"Orphan Media recovery deferred; raw cursor advanced");
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(ids.len())
    }
}
