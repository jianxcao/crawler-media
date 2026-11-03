//! Subscribe configuration projection; lifecycle order is owned by Subscribe.
use crate::{JobError, Queue};
use rusqlite::{OptionalExtension, params};
impl Queue {
    pub fn apply_subscribe_config(
        &self,
        id: domain::SubscribeId,
        generation: &str,
        epoch: i64,
        revision: i64,
        work: impl FnOnce(&Self) -> Result<(), JobError>,
    ) -> Result<bool, JobError> {
        self.with_connection(|conn| {
            let tx=rusqlite::Transaction::new_unchecked(conn,rusqlite::TransactionBehavior::Immediate)?;
            let fence=read_fence(&tx,id)?;
            if let Some((known,incarnation,applied,closed))=fence.as_ref() {
                if *known>epoch || (*known==epoch && (incarnation!=generation || *closed || *applied>revision)) {return Ok(false);}
            }
            let receipt=tx.query_row("SELECT revision FROM subscribe_config_receipts WHERE subscribe_id=?1 AND generation=?2",params![id.to_string(),generation],|r|r.get::<_,i64>(0)).optional()?;
            if receipt.is_some_and(|applied|applied>=revision) {return Ok(false);}
            if fence.as_ref().is_none_or(|(known,_,_,_)|*known<epoch) {
                self.delete_def(crate::JobIdentity::subscribe_search(id))?;
            }
            work(self)?;
            tx.execute("INSERT INTO subscribe_lifecycle_fences VALUES(?1,?2,?3,?4,0) ON CONFLICT(subscribe_id) DO UPDATE SET epoch=excluded.epoch,generation=excluded.generation,revision=excluded.revision,closed=0",params![id.to_string(),epoch,generation,revision])?;
            tx.execute("INSERT INTO subscribe_config_receipts VALUES(?1,?2,?3) ON CONFLICT(subscribe_id,generation) DO UPDATE SET revision=excluded.revision",params![id.to_string(),generation,revision])?;
            tx.commit()?;Ok(true)
        })
    }
    pub fn close_subscribe_epoch(
        &self,
        id: domain::SubscribeId,
        generation: &str,
        epoch: i64,
    ) -> Result<bool, JobError> {
        self.with_connection(|conn| {
            let tx=rusqlite::Transaction::new_unchecked(conn,rusqlite::TransactionBehavior::Immediate)?;
            if let Some((known,incarnation,_,_))=read_fence(&tx,id)? {
                if known>epoch || (known==epoch && incarnation!=generation) {return Ok(false);}
            }
            self.delete_def(crate::JobIdentity::subscribe_search(id))?;
            tx.execute("INSERT INTO subscribe_lifecycle_fences VALUES(?1,?2,?3,0,1) ON CONFLICT(subscribe_id) DO UPDATE SET epoch=excluded.epoch,generation=excluded.generation,revision=CASE WHEN subscribe_lifecycle_fences.epoch=excluded.epoch THEN subscribe_lifecycle_fences.revision ELSE 0 END,closed=1",params![id.to_string(),epoch,generation])?;
            tx.commit()?;Ok(true)
        })
    }
}
fn read_fence(
    conn: &rusqlite::Connection,
    id: domain::SubscribeId,
) -> Result<Option<(i64, String, i64, bool)>, JobError> {
    Ok(conn.query_row("SELECT epoch,generation,revision,closed FROM subscribe_lifecycle_fences WHERE subscribe_id=?1",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?)
}
