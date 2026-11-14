//! Durable Subscribe creation and release. Each stage owns one database only.
use crate::{Db, Store, StoreError};
use domain::{MediaId, Subscribe, SubscribeId};
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct SubscribeCreation {
    pub(crate) id: SubscribeId,
    pub(crate) generation: String,
    pub(crate) epoch: i64,
    pub(crate) media_id: MediaId,
}
impl SubscribeCreation {
    pub fn subscribe_id(&self) -> SubscribeId {
        self.id
    }
}
impl Store {
    pub fn begin_subscribe_creation(
        &self,
        subscribe: &Subscribe,
    ) -> Result<SubscribeCreation, StoreError> {
        self.begin_subscribe_creation_with_compensation(subscribe, None)
    }
    pub(crate) fn begin_subscribe_creation_with_compensation(
        &self,
        subscribe: &Subscribe,
        snapshot: Option<String>,
    ) -> Result<SubscribeCreation, StoreError> {
        let generation = SubscribeId::new().to_string();
        self.with_writer(Db::Subscribe,|conn| {
            let tx=crate::write_tx(conn)?;
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM subscribes WHERE id=?1)",[subscribe.id.to_string()],|r|r.get::<_,bool>(0))? {
                return Err(StoreError::Protected(format!("Subscribe {} already exists",subscribe.id)));
            }
            tx.execute("INSERT INTO subscribe_reference_ops(subscribe_id,generation,media_id,state,release_mode,compensation_json) VALUES(?1,?2,?3,'creating',?4,?5)",params![subscribe.id.to_string(),generation,subscribe.media_id.to_string(),if snapshot.is_some(){"compensate"}else{"ordinary"},snapshot])?;
            let epoch=tx.last_insert_rowid();
            tx.commit()?;
            Ok(SubscribeCreation{id:subscribe.id,generation,epoch,media_id:subscribe.media_id})
        })
    }
    pub fn retain_subscribe_creation(&self, op: &SubscribeCreation) -> Result<(), StoreError> {
        self.with_writer(Db::Library,|conn| {
            let tx=crate::write_tx(conn)?;
            let state=tx.query_row("SELECT identity_state FROM media WHERE media_id=?1",[op.media_id.to_string()],|r|r.get::<_,String>(0)).optional()?;
            match state.as_deref() {
                None=>return Err(StoreError::Missing(format!("Media {}",op.media_id))),
                Some("merging"|"retired")=>return Err(StoreError::Protected(format!("Media {} frozen",op.media_id))),
                _=>{}
            }
            tx.execute("INSERT INTO subscribe_retention_fences VALUES(?1,?2,'open') ON CONFLICT DO NOTHING",params![op.id.to_string(),op.generation])?;
            let open:bool=tx.query_row("SELECT state='open' FROM subscribe_retention_fences WHERE subscribe_id=?1 AND generation=?2",params![op.id.to_string(),op.generation],|r|r.get(0))?;
            if !open {return Err(StoreError::Protected("Subscribe generation closed".into()));}
            tx.execute("INSERT OR IGNORE INTO subscribe_retention VALUES(?1,?2,?3)",params![op.id.to_string(),op.generation,op.media_id.to_string()])?;
            tx.commit()?;Ok(())
        })?;
        self.with_writer(Db::Subscribe, |conn| {
            let n=conn.execute("UPDATE subscribe_reference_ops SET state='retained' WHERE epoch=?1 AND generation=?2 AND state IN ('creating','retained')",params![op.epoch,op.generation])?;
            if n!=1 {return Err(StoreError::Protected("Subscribe creation aborted before retention acknowledgment".into()));}
            Ok(())
        })
    }
    pub fn commit_subscribe_creation(
        &self,
        op: &SubscribeCreation,
        subscribe: &Subscribe,
        check_overlap: bool,
    ) -> Result<(), StoreError> {
        if op.id != subscribe.id || op.media_id != subscribe.media_id {
            return Err(StoreError::Invalid(
                "Subscribe creation identity mismatch".into(),
            ));
        }
        self.with_writer(Db::Subscribe,|conn| {
            let tx=crate::write_tx(conn)?;
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM subscribe_merge_fences WHERE media_id=?1)",[op.media_id.to_string()],|r|r.get::<_,bool>(0))? {return Err(StoreError::Protected("Subscribe Media merge fenced".into()));}
            let n=tx.execute("UPDATE subscribe_reference_ops SET state='committed' WHERE epoch=?1 AND subscribe_id=?2 AND generation=?3 AND state='retained'",params![op.epoch,op.id.to_string(),op.generation])?;
            if n!=1 {return Err(StoreError::Protected("Subscribe creation no longer creating".into()));}
            if check_overlap {crate::subscribes::admission::ensure_nonoverlapping(&tx,subscribe)?;}
            crate::subscribes::admission::insert_row(&tx,subscribe,&op.generation,op.epoch)?;
            tx.execute("UPDATE subscribes SET jobs_sync_pending=1 WHERE id=?1",[op.id.to_string()])?;
            tx.commit()?;Ok(())
        })
    }
    pub fn abort_subscribe_creation(&self, op: &SubscribeCreation) -> Result<(), StoreError> {
        let abort=self.with_writer(Db::Subscribe,|conn| {
            conn.execute("UPDATE subscribe_reference_ops SET state='aborting' WHERE epoch=?1 AND generation=?2 AND state IN ('creating','retained')",params![op.epoch,op.generation])?;
            conn.execute("INSERT INTO subscribe_jobs_cleanup(id,generation) SELECT subscribe_id,generation FROM subscribe_reference_ops WHERE epoch=?1 AND state IN ('aborting','releasing') ON CONFLICT(id) DO UPDATE SET generation=excluded.generation WHERE (SELECT epoch FROM subscribe_reference_ops WHERE generation=excluded.generation)>=(SELECT epoch FROM subscribe_reference_ops WHERE generation=subscribe_jobs_cleanup.generation)",[op.epoch])?;
            Ok(conn.query_row("SELECT state IN ('aborting','releasing') FROM subscribe_reference_ops WHERE epoch=?1 AND generation=?2",params![op.epoch,op.generation],|r|r.get::<_,bool>(0)).optional()?.unwrap_or(false))
        })?;
        if abort {
            self.close_subscribe_retention(op)?;
        }
        Ok(())
    }
    pub(crate) fn close_subscribe_retention(
        &self,
        op: &SubscribeCreation,
    ) -> Result<(), StoreError> {
        let compensation = self.subscribe_compensation(op)?;
        let remains=self.with_writer(Db::Library,|conn| {
            let tx=crate::write_tx(conn)?;
            let closed = tx.query_row("SELECT state='closed' FROM subscribe_retention_fences WHERE subscribe_id=?1 AND generation=?2",params![op.id.to_string(),op.generation],|r|r.get::<_,bool>(0)).optional()?.unwrap_or(false);
            let protected = if let Some(snapshot) = &compensation {
                if closed {
                    let protected = tx.query_row("SELECT protected FROM subscribe_compensation_receipts WHERE generation=?1",[&op.generation],|r|r.get::<_,bool>(0))?;
                    let changed = !protected && crate::media::compensation_media_changed(&tx, snapshot)?;
                    if changed { tx.execute("UPDATE subscribe_compensation_receipts SET protected=1 WHERE generation=?1",[&op.generation])?; }
                    protected || changed
                } else {
                    let protected = crate::media::restore_compensation_in_tx(&tx, snapshot)?;
                    tx.execute("INSERT INTO subscribe_compensation_receipts VALUES(?1,?2)",params![op.generation,protected])?;
                    protected
                }
            } else { false };
            tx.execute("INSERT INTO subscribe_retention_fences VALUES(?1,?2,'closed') ON CONFLICT(subscribe_id,generation) DO UPDATE SET state='closed'",params![op.id.to_string(),op.generation])?;
            let mut stmt=tx.prepare("SELECT media_id FROM subscribe_retention WHERE subscribe_id=?1 AND generation=?2 LIMIT 64")?;
            let ids=stmt.query_map(params![op.id.to_string(),op.generation],|r|r.get::<_,String>(0))?.map(|r|Ok(r?.parse()?)).collect::<Result<HashSet<MediaId>,StoreError>>()?;
            drop(stmt);
            for id in &ids {tx.execute("DELETE FROM subscribe_retention WHERE subscribe_id=?1 AND generation=?2 AND media_id=?3",params![op.id.to_string(),op.generation,id.to_string()])?;}
            let mut candidates = ids;
            let remains:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM subscribe_retention WHERE subscribe_id=?1 AND generation=?2)",params![op.id.to_string(),op.generation],|r|r.get(0))?;
            if compensation.is_some() && !protected && !remains { candidates.insert(op.media_id); }
            if protected { candidates.remove(&op.media_id); }
            crate::library_write::orphan_media::purge_orphan_media_in_tx(&tx,&candidates)?;
            tx.commit()?;Ok(remains)
        })?;
        if remains {
            return Ok(());
        }
        self.with_writer(Db::Subscribe,|conn| {conn.execute("UPDATE subscribe_reference_ops SET state='done' WHERE epoch=?1 AND generation=?2 AND state IN ('aborting','releasing')",params![op.epoch,op.generation])?;Ok(())})
    }
    pub fn pending_subscribe_references(
        &self,
        limit: usize,
    ) -> Result<Vec<Result<SubscribeCreation, StoreError>>, StoreError> {
        let pending=self.with_subscribe_read(|db| {
            let conn=&db.subscribe;
            let mut after:i64=conn.query_row("SELECT epoch FROM subscribe_recovery_cursor WHERE id=1",[],|r|r.get(0))?;
            let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM subscribe_reference_ops WHERE state IN ('creating','retained','aborting','releasing') AND epoch>?1)",[after],|r|r.get(0))?;
            if !exists {after=0;}
            let mut stmt=conn.prepare("SELECT subscribe_id,generation,epoch,media_id FROM subscribe_reference_ops WHERE state IN ('creating','retained','aborting','releasing') AND epoch>?1 ORDER BY epoch LIMIT ?2")?;
            let pending=stmt.query_map(params![after,limit.min(64) as i64],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?)))?.collect::<Result<Vec<_>,_>>()?;
            Ok(pending)
        })?;
        self.with_writer(Db::Subscribe, |conn| {
            conn.execute(
                "UPDATE subscribe_recovery_cursor SET epoch=?1 WHERE id=1",
                [pending.last().map_or(0, |row| row.2)],
            )?;
            Ok(())
        })?;
        Ok(pending
            .into_iter()
            .map(|(id, generation, epoch, media)| {
                Ok(SubscribeCreation {
                    id: id.parse()?,
                    generation,
                    epoch,
                    media_id: media.parse()?,
                })
            })
            .collect())
    }
    pub fn recover_subscribe_references(&self, limit: usize) -> Result<usize, StoreError> {
        let pending = self.pending_subscribe_references(limit)?;
        let mut failure = None;
        for candidate in &pending {
            let op = match candidate {
                Ok(op) => op,
                Err(error) => {
                    tracing::error!(%error, "Invalid Subscribe reference recovery candidate; raw cursor advanced");
                    failure = Some(StoreError::Invalid(error.to_string()));
                    continue;
                }
            };
            if let Err(error) = self.abort_subscribe_creation(op) {
                tracing::error!(subscribe_id=%op.id,generation=%op.generation,epoch=op.epoch,media=%op.media_id,%error,"Subscribe reference recovery deferred; raw cursor advanced");
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(pending.len())
    }
}
