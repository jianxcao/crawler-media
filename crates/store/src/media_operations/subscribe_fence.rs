//! Subscribe-side merge ownership prevents pre-freeze creators committing late.
use crate::{Db, Store, StoreError};
use domain::MediaId;
use rusqlite::params;
impl Store {
    pub(super) fn install_subscribe_merge_fence(
        &self,
        op: &str,
        survivor: MediaId,
        loser: MediaId,
    ) -> Result<(), StoreError> {
        self.with_writer(Db::Subscribe,|conn| {
            let tx=crate::write_tx(conn)?;
            tx.execute("INSERT OR IGNORE INTO subscribe_merge_controls VALUES(?1,'active')",[op])?;
            let active:bool=tx.query_row("SELECT state='active' FROM subscribe_merge_controls WHERE operation_id=?1",[op],|r|r.get(0))?;
            if active {
                for id in [survivor,loser] {
                    tx.execute("INSERT INTO subscribe_merge_fences VALUES(?1,?2) ON CONFLICT DO NOTHING",params![id.to_string(),op])?;
                    let owner:String=tx.query_row("SELECT operation_id FROM subscribe_merge_fences WHERE media_id=?1",[id.to_string()],|r|r.get(0))?;
                    if owner!=op {return Err(StoreError::Protected(format!("Media {id} fenced by merge {owner}")));}
                }
            tx.execute("UPDATE subscribe_reference_ops SET state='aborting' WHERE media_id IN (?1,?2) AND state IN ('creating','retained')",params![survivor.to_string(),loser.to_string()])?;
            }
            tx.commit()?;Ok(())
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
