//! Durable rollback of a failed Subscribe creation, distinct from user deletion.
use crate::{Db, Store, StoreError, SubscribeCreation};
use domain::{Media, Subscribe};
use rusqlite::{OptionalExtension, params};

pub(crate) struct Compensation {
    pub(crate) expected: Media,
    pub(crate) previous: Option<Media>,
}

impl Store {
    pub fn begin_compensatable_subscribe_creation(
        &self,
        subscribe: &Subscribe,
        expected: &Media,
        previous: Option<&Media>,
    ) -> Result<SubscribeCreation, StoreError> {
        if expected.id != subscribe.media_id || previous.is_some_and(|old| old.id != expected.id) {
            return Err(StoreError::Invalid(
                "Subscribe compensation identity mismatch".into(),
            ));
        }
        let snapshot = serde_json::to_string(&(
            MediaSnapshot::from(expected),
            previous.map(MediaSnapshot::from),
        ))?;
        self.begin_subscribe_creation_with_compensation(subscribe, Some(snapshot))
    }

    pub fn rollback_subscribe_creation(&self, op: &SubscribeCreation) -> Result<(), StoreError> {
        let pending = self.with_writer(Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            // The exact generation owns its rollback. A stale caller cannot delete
            // a replacement, or turn an already ordinary release into compensation.
            tx.execute("UPDATE subscribe_reference_ops SET state=CASE WHEN state='committed' THEN 'releasing' ELSE 'aborting' END WHERE epoch=?1 AND generation=?2 AND release_mode='compensate' AND state IN ('creating','retained','committed')",params![op.epoch,op.generation])?;
            let pending = tx.query_row("SELECT EXISTS(SELECT 1 FROM subscribe_reference_ops WHERE epoch=?1 AND generation=?2 AND release_mode='compensate' AND state IN ('aborting','releasing'))",params![op.epoch,op.generation],|r|r.get::<_,bool>(0))?;
            if pending {
                tx.execute("INSERT INTO subscribe_jobs_cleanup(id,generation) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET generation=excluded.generation WHERE (SELECT epoch FROM subscribe_reference_ops WHERE generation=excluded.generation)>=(SELECT epoch FROM subscribe_reference_ops WHERE generation=subscribe_jobs_cleanup.generation)",params![op.id.to_string(),op.generation])?;
                tx.execute("DELETE FROM subscribes WHERE id=?1 AND jobs_sync_generation=?2 AND lifecycle_epoch=?3",params![op.id.to_string(),op.generation,op.epoch])?;
            }
            tx.commit()?;
            Ok(pending)
        })?;
        if pending {
            self.close_subscribe_retention(op)?;
        }
        Ok(())
    }

    /// Successful publication makes any future user deletion an ordinary release.
    /// Failure leaves a committed Subscribe with its durable Jobs activation; recovery
    /// never aborts committed business rows, so callers report acknowledgment pending.
    pub fn finalize_subscribe_creation(&self, op: &SubscribeCreation) -> Result<(), StoreError> {
        self.with_writer(Db::Subscribe, |conn| {
            let changed = conn.execute("UPDATE subscribe_reference_ops SET release_mode='ordinary',compensation_json=NULL WHERE epoch=?1 AND generation=?2 AND state='committed'",params![op.epoch,op.generation])?;
            if changed != 1 { return Err(StoreError::Protected("Subscribe creation no longer committed".into())); }
            Ok(())
        })
    }

    pub(crate) fn subscribe_compensation(
        &self,
        op: &SubscribeCreation,
    ) -> Result<Option<Compensation>, StoreError> {
        self.with_subscribe_read(|db| {
            let snapshot: Option<String> = db.subscribe.query_row("SELECT compensation_json FROM subscribe_reference_ops WHERE epoch=?1 AND generation=?2 AND release_mode='compensate' AND state IN ('aborting','releasing')",params![op.epoch,op.generation],|r|r.get(0)).optional()?.flatten();
            snapshot.map(|json| {
                let (expected, previous): (MediaSnapshot, Option<MediaSnapshot>) = serde_json::from_str(&json)?;
                Ok(Compensation { expected: expected.into(), previous: previous.map(Into::into) })
            }).transpose()
        })
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MediaSnapshot {
    id: domain::MediaId,
    kind: domain::MediaKind,
    title: String,
    year: Option<u16>,
    original_title: Option<String>,
    aliases: Vec<String>,
    tmdb_id: Option<String>,
    douban_id: Option<String>,
    tvdb_id: Option<String>,
    bangumi_id: Option<String>,
    anilist_id: Option<String>,
}
impl From<&Media> for MediaSnapshot {
    fn from(media: &Media) -> Self {
        Self {
            id: media.id,
            kind: media.kind,
            title: media.title.clone(),
            year: media.year,
            original_title: media.original_title.clone(),
            aliases: media.aliases.clone(),
            tmdb_id: media.tmdb_id.clone(),
            douban_id: media.douban_id.clone(),
            tvdb_id: media.tvdb_id.clone(),
            bangumi_id: media.bangumi_id.clone(),
            anilist_id: media.anilist_id.clone(),
        }
    }
}
impl From<MediaSnapshot> for Media {
    fn from(media: MediaSnapshot) -> Self {
        Self {
            id: media.id,
            kind: media.kind,
            title: media.title,
            year: media.year,
            original_title: media.original_title,
            aliases: media.aliases,
            tmdb_id: media.tmdb_id,
            douban_id: media.douban_id,
            tvdb_id: media.tvdb_id,
            bangumi_id: media.bangumi_id,
            anilist_id: media.anilist_id,
        }
    }
}
