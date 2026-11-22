//! Store owns unique writers; borrowed SQL scopes never reacquire a database.
use crate::{Db, ReaderPool, Store, StoreError};
use rusqlite::Connection;
use std::{num::NonZeroUsize, sync::Arc};

pub struct LibraryConnection<'a> {
    pub(crate) library: &'a Connection,
    pub(crate) data_dir: &'a std::path::Path,
    pub(crate) authorization: Option<(domain::UserId, domain::UserRole)>,
    pub(crate) root_ledger_scans: &'a std::sync::atomic::AtomicUsize,
    pub(crate) media_ledger_reads: &'a std::sync::atomic::AtomicUsize,
}
pub struct SubscribeConnection<'a> {
    pub(crate) subscribe: &'a Connection,
}
pub(crate) struct CacheConnection<'a> {
    pub(crate) cache: &'a Connection,
}

pub(crate) struct Readers {
    pub(crate) app: Arc<ReaderPool>,
    pub(crate) subscribe: Arc<ReaderPool>,
    pub(crate) library: Arc<ReaderPool>,
    pub(crate) cache: Arc<ReaderPool>,
}
impl Store {
    pub fn reader_pool(&self, db: Db) -> Arc<ReaderPool> {
        Arc::clone(match db {
            Db::App => &self.readers.app,
            Db::Subscribe => &self.readers.subscribe,
            Db::Library => &self.readers.library,
            Db::Cache => &self.readers.cache,
        })
    }
    #[doc(hidden)]
    pub fn configure_reader_pool_size(&self, db: Db, size: NonZeroUsize) -> Result<(), StoreError> {
        self.reader_pool(db).set_capacity(size)
    }
    pub fn with_writer<T>(
        &self,
        db: Db,
        work: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.writers.with_writer(db, work)
    }
    /// Borrow a database's single writer strictly for storage/schema maintenance
    /// (`PRAGMA optimize`, `incremental_vacuum`, `VACUUM`) that never mutates logical
    /// or actor/auth rows.
    pub fn with_maintenance_writer<T>(
        &self,
        db: Db,
        work: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.writers.with_maintenance_writer(db, work)
    }
    pub(crate) fn library_connection<'a>(
        &'a self,
        library: &'a Connection,
    ) -> LibraryConnection<'a> {
        LibraryConnection {
            library,
            data_dir: &self.data_dir,
            authorization: None,
            root_ledger_scans: &self.root_ledger_scans,
            media_ledger_reads: &self.media_ledger_reads,
        }
    }
    pub fn library_write<T>(
        &self,
        work: impl FnOnce(&LibraryConnection<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.library_write_with_error(std::convert::identity, work)
    }
    pub fn library_write_with_error<T, E>(
        &self,
        map_store_error: impl FnOnce(StoreError) -> E,
        work: impl FnOnce(&LibraryConnection<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        self.with_writer(Db::Library, |conn| {
            let aliases = crate::path_key::AliasScope::new(conn);
            let tx = crate::write_tx(conn)?;
            let value = work(&self.library_connection(&tx));
            if value.is_ok() {
                tx.commit()?;
                aliases.publish()?;
            }
            Ok(value)
        })
        .map_err(map_store_error)?
    }

    pub(crate) fn with_library_read<T>(
        &self,
        work: impl FnOnce(&LibraryConnection<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.with_read(Db::Library, |conn| work(&self.library_connection(conn)))
    }
}

const MAX_ACTOR_VERIFY_ATTEMPTS: usize = 32;

pub(crate) enum VerifiedActorOutcome<T> {
    Applied(T),
    InactiveUser,
    RevokedDevice,
}

impl Store {
    pub(crate) fn with_verified_actor_library_write<T>(
        &self,
        user_id: domain::UserId,
        device_id: Option<&str>,
        work: impl FnOnce(&LibraryConnection<'_>) -> Result<T, StoreError>,
    ) -> Result<VerifiedActorOutcome<T>, StoreError> {
        self.with_verified_actor_library_write_observed(user_id, device_id, || {}, work)
    }

    pub(crate) fn with_verified_actor_library_write_observed<T>(
        &self,
        user_id: domain::UserId,
        device_id: Option<&str>,
        before_writer: impl FnOnce(),
        work: impl FnOnce(&LibraryConnection<'_>) -> Result<T, StoreError>,
    ) -> Result<VerifiedActorOutcome<T>, StoreError> {
        let mut deadline = std::time::Instant::now()
            + crate::Writers::remaining_write_wait(std::time::Duration::from_secs(1));
        let mut before_writer = Some(before_writer);
        let mut work = Some(work);
        for _attempt in 0..MAX_ACTOR_VERIFY_ATTEMPTS {
            let seq_before = self.writers.wait_for_even_app_seq(deadline)?;
            let (role, revoked) = self.with_app_read(|app| {
                let role = crate::app_auth::active_user_role_on(app.app, user_id)?;
                let revoked = match device_id {
                    Some(dev) => crate::app_auth::is_device_revoked_on(app.app, user_id, dev)?,
                    None => false,
                };
                Ok((role, revoked))
            })?;
            if self.writers.app_seq() != seq_before {
                continue;
            }
            let Some(role) = role else {
                return Ok(VerifiedActorOutcome::InactiveUser);
            };
            if revoked {
                return Ok(VerifiedActorOutcome::RevokedDevice);
            }
            if let Some(hook) = before_writer.take() {
                hook();
                deadline = std::time::Instant::now()
                    + crate::Writers::remaining_write_wait(std::time::Duration::from_secs(1));
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(StoreError::WriteBusy);
            }
            let applied = self
                .writers
                .with_writer_timeout(remaining, Db::Library, |conn| {
                    if self.writers.app_seq() != seq_before {
                        return Ok(None);
                    }
                    let work = work
                        .take()
                        .ok_or_else(|| StoreError::Invalid("actor write callback reused".into()))?;
                    let mut db = self.library_connection(conn);
                    db.authorization = Some((user_id, role));
                    work(&db).map(Some)
                })?;
            if let Some(value) = applied {
                return Ok(VerifiedActorOutcome::Applied(value));
            }
        }
        Err(StoreError::WriteBusy)
    }

    pub(crate) fn with_library_write<T>(
        &self,
        work: impl FnOnce(&LibraryConnection<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.with_writer(Db::Library, |conn| work(&self.library_connection(conn)))
    }
    pub(crate) fn with_subscribe_read<T>(
        &self,
        work: impl FnOnce(&SubscribeConnection<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.with_read(Db::Subscribe, |subscribe| {
            work(&SubscribeConnection { subscribe })
        })
    }
    pub(crate) fn with_subscribe_write<T>(
        &self,
        work: impl FnOnce(&SubscribeConnection<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.with_writer(Db::Subscribe, |subscribe| {
            work(&SubscribeConnection { subscribe })
        })
    }
    pub(crate) fn with_cache_read<T>(
        &self,
        work: impl FnOnce(&CacheConnection<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.with_read(Db::Cache, |cache| work(&CacheConnection { cache }))
    }
    pub(crate) fn with_cache_write<T>(
        &self,
        work: impl FnOnce(&CacheConnection<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.with_writer(Db::Cache, |cache| work(&CacheConnection { cache }))
    }
}

impl LibraryConnection<'_> {
    pub(crate) fn user_role_observation(
        &self,
        user: domain::UserId,
    ) -> Result<domain::UserRole, StoreError> {
        self.authorization
            .filter(|(owner, _)| *owner == user)
            .map(|(_, role)| role)
            .ok_or_else(|| {
                StoreError::Invalid(
                    "playback scope is missing its owned authorization observation".into(),
                )
            })
    }
    pub fn recorded_path_key(&self, path: &str) -> String {
        crate::path_key::canonical_recorded_key(self.library, path)
    }
}
