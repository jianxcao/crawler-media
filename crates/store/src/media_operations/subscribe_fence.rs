//! Subscribe-side merge ownership prevents prefrozen creators committing late.
use crate::{Db, Store, StoreError};
use domain::MediaId;
use rusqlite::{Connection, OptionalExtension, params};

const ABORT_WINDOW: usize = 64;

impl Store {
    pub(super) fn install_subscribe_merge_fence(
        &self,
        op: &str,
        survivor: MediaId,
        loser: MediaId,
    ) -> Result<(), StoreError> {
        // Commit exclusion first. A later abort-window failure must never reopen
        // the prefrozen token's Subscribe commit gate.
        let active = self.with_writer(Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            tx.execute(
                "INSERT OR IGNORE INTO subscribe_merge_controls VALUES(?1,'active')",
                [op],
            )?;
            if !control_active(&tx, op)? {
                return Ok(false);
            }
            for id in [survivor, loser] {
                tx.execute(
                    "INSERT INTO subscribe_merge_fences VALUES(?1,?2) ON CONFLICT DO NOTHING",
                    params![id.to_string(), op],
                )?;
            }
            verify_owners(&tx, op, survivor, loser)?;
            tx.commit()?;
            Ok(true)
        })?;
        if active {
            // One Media range per seek avoids sorting all pending epochs across
            // two ranges before LIMIT. Every window releases its actual writer.
            for media in [survivor, loser] {
                while self.abort_merge_creators(op, survivor, loser, media)? {}
            }
        }
        Ok(())
    }

    fn abort_merge_creators(
        &self,
        op: &str,
        survivor: MediaId,
        loser: MediaId,
        media: MediaId,
    ) -> Result<bool, StoreError> {
        self.with_writer(Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            if !control_active(&tx, op)? {
                return Ok(false);
            }
            verify_owners(&tx, op, survivor, loser)?;
            let mut stmt = tx.prepare(
                "SELECT epoch,generation FROM subscribe_reference_ops
                 WHERE media_id=?1 AND state IN ('creating','retained')
                 ORDER BY epoch LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![media.to_string(), ABORT_WINDOW as i64], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            for (epoch, generation) in &rows {
                let changed = tx.execute(
                    "UPDATE subscribe_reference_ops SET state='aborting'
                     WHERE epoch=?1 AND generation=?2 AND media_id=?3
                       AND state IN ('creating','retained')",
                    params![epoch, generation, media.to_string()],
                )?;
                if changed != 1 {
                    return Err(StoreError::Protected(
                        "Subscribe creator changed during merge abort".into(),
                    ));
                }
            }
            tx.commit()?;
            Ok(!rows.is_empty())
        })
    }

    pub(super) fn finish_subscribe_merge_fence(&self, op: &str) -> Result<(), StoreError> {
        self.with_writer(Db::Subscribe,|conn| {
            let tx=crate::write_tx(conn)?;
            tx.execute("UPDATE subscribe_merge_controls SET state='finished' WHERE operation_id=?1 AND state='active'",[op])?;
            tx.execute("DELETE FROM subscribe_merge_fences WHERE operation_id=?1",[op])?;
            tx.commit()?;Ok(())
        })
    }
}

fn control_active(conn: &Connection, op: &str) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM subscribe_merge_controls WHERE operation_id=?1 AND state='active')",
        [op], |r| r.get(0),
    )?)
}
fn verify_owners(
    conn: &Connection,
    op: &str,
    survivor: MediaId,
    loser: MediaId,
) -> Result<(), StoreError> {
    for id in [survivor, loser] {
        let owner = conn
            .query_row(
                "SELECT operation_id FROM subscribe_merge_fences WHERE media_id=?1",
                [id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if owner.as_deref() != Some(op) {
            return Err(StoreError::Protected(format!(
                "Media {id} merge fence no longer owned by {op}"
            )));
        }
    }
    Ok(())
}
