//! A deleting Collection's memberships are its durable bounded cleanup queue.
use crate::{Db, Store, StoreError};
use rusqlite::{Connection, params};
use std::collections::HashSet;

pub(super) fn ensure_mutable(conn: &Connection, id: &str) -> Result<(), StoreError> {
    let deleting: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM collections WHERE id=?1 AND deleting=1)",
        [id],
        |r| r.get(0),
    )?;
    if deleting {
        return Err(StoreError::Protected(format!("Collection {id} deleting")));
    }
    Ok(())
}
impl Store {
    pub fn advance_collection_deletion(&self, id: &str) -> Result<bool, StoreError> {
        self.with_writer(Db::Library,|conn| {
            let tx=crate::write_tx(conn)?;
            let deleting:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM collections WHERE id=?1 AND deleting=1)",[id],|r|r.get(0))?;
            if !deleting {return Ok(true);}
            let mut stmt=tx.prepare("SELECT media_id FROM collection_items WHERE collection_id=?1 ORDER BY media_id LIMIT 64")?;
            let ids=stmt.query_map([id],|r|r.get::<_,String>(0))?.map(|r|Ok(r?.parse()?)).collect::<Result<HashSet<domain::MediaId>,StoreError>>()?;
            drop(stmt);
            for media in &ids {
                crate::library_operations::integrity_guard::ensure_media_integrity_writable(&tx,*media)?;
                tx.execute("DELETE FROM collection_items WHERE collection_id=?1 AND media_id=?2",params![id,media.to_string()])?;
                crate::library_write::reduce::reduce_media_everywhere(&tx,*media)?;
            }
            crate::library_write::orphan_media::purge_orphan_media_in_tx(&tx,&ids)?;
            let remains:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM collection_items WHERE collection_id=?1)",[id],|r|r.get(0))?;
            if !remains {tx.execute("DELETE FROM collections WHERE id=?1 AND deleting=1",[id])?;}
            tx.commit()?;Ok(!remains)
        })
    }
    pub fn recover_collection_deletions(&self) -> Result<usize, StoreError> {
        let ids = self.with_writer(Db::Library, |conn| {
            let tx = crate::write_tx(conn)?;
            let mut after: String = tx.query_row(
                "SELECT cursor FROM reference_recovery_cursors WHERE kind='collection'",
                [],
                |r| r.get(0),
            )?;
            let more: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM collections WHERE deleting=1 AND id>?1)",
                [&after],
                |r| r.get(0),
            )?;
            if !more {
                after.clear();
            }
            let mut stmt = tx.prepare(
                "SELECT id FROM collections WHERE deleting=1 AND id>?1 ORDER BY id LIMIT 64",
            )?;
            let ids = stmt
                .query_map([after], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.execute(
                "UPDATE reference_recovery_cursors SET cursor=?1 WHERE kind='collection'",
                [ids.last().map_or("", String::as_str)],
            )?;
            tx.commit()?;
            Ok(ids)
        })?;
        for id in &ids {
            if let Err(error) = self.advance_collection_deletion(id) {
                tracing::warn!(collection_id=%id,%error,"Collection deletion remains pending");
            }
        }
        Ok(ids.len())
    }
}
