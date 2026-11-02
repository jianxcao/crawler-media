//! Durable, revisioned Subscribe configuration intent for Jobs synchronization.
use crate::{Store, StoreError};
use domain::{Subscribe, SubscribeId};
use rusqlite::OptionalExtension;

pub struct SubscribeJobSync {
    pub subscribe: Subscribe,
    pub generation: String,
    pub revision: i64,
    pub create: bool,
}

impl Store {
    pub fn pending_subscribe_job_sync(
        &self,
        after: Option<SubscribeId>,
        limit: usize,
    ) -> Result<Vec<SubscribeId>, StoreError> {
        self.with_subscribe_read(|db| {
            let mut query = db.subscribe.prepare(
                "SELECT id FROM subscribes WHERE jobs_sync_pending=1 AND id>?1 ORDER BY id LIMIT ?2")?;
            query.query_map(rusqlite::params![after.map(|id| id.to_string()).unwrap_or_default(), limit.min(64) as i64], |row| row.get::<_, String>(0))?
                .map(|row| Ok(row?.parse()?)).collect()
        })
    }

    pub fn subscribe_job_sync(
        &self,
        id: SubscribeId,
    ) -> Result<Option<SubscribeJobSync>, StoreError> {
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let connection = crate::SubscribeConnection { subscribe: &tx };
            let status = tx.query_row(
                "SELECT jobs_sync_generation,jobs_sync_revision,jobs_sync_create FROM subscribes WHERE id=?1 AND jobs_sync_pending=1",
                [id.to_string()], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, bool>(2)?))).optional()?;
            let value = match status {
                Some((generation, revision, create)) => connection.get_subscribe(id)?.map(|subscribe|
                    SubscribeJobSync { subscribe, generation, revision, create }),
                None => None,
            };
            tx.commit()?;
            Ok(value)
        })
    }

    pub fn acknowledge_subscribe_job_sync(
        &self,
        id: SubscribeId,
        generation: &str,
        revision: i64,
    ) -> Result<bool, StoreError> {
        self.with_subscribe_write(|db| Ok(db.subscribe.execute(
            "UPDATE subscribes SET jobs_sync_pending=0,jobs_sync_create=0 WHERE id=?1 AND jobs_sync_generation=?2 AND jobs_sync_revision=?3 AND jobs_sync_pending=1",
            rusqlite::params![id.to_string(),generation,revision])? > 0))
    }
}

impl Store {
    pub fn pending_subscribe_jobs_cleanup(
        &self,
        after: Option<SubscribeId>,
        limit: usize,
    ) -> Result<Vec<(SubscribeId, String)>, StoreError> {
        self.with_subscribe_read(|db| {
            let mut query = db.subscribe.prepare(
                "SELECT id,generation FROM subscribe_jobs_cleanup WHERE id>?1 ORDER BY id LIMIT ?2",
            )?;
            query
                .query_map(
                    rusqlite::params![
                        after.map(|id| id.to_string()).unwrap_or_default(),
                        limit.min(64) as i64
                    ],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )?
                .map(|row| {
                    let (id, generation) = row?;
                    Ok((id.parse()?, generation))
                })
                .collect()
        })
    }
    pub fn subscribe_jobs_cleanup(&self, id: SubscribeId) -> Result<Option<String>, StoreError> {
        self.with_subscribe_read(|db| {
            Ok(db
                .subscribe
                .query_row(
                    "SELECT generation FROM subscribe_jobs_cleanup WHERE id=?1",
                    [id.to_string()],
                    |row| row.get(0),
                )
                .optional()?)
        })
    }
    pub fn acknowledge_subscribe_jobs_cleanup(
        &self,
        id: SubscribeId,
        generation: &str,
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| {
            db.subscribe.execute(
                "DELETE FROM subscribe_jobs_cleanup WHERE id=?1 AND generation=?2",
                rusqlite::params![id.to_string(), generation],
            )?;
            Ok(())
        })
    }
}
