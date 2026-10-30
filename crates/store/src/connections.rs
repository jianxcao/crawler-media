//! Store owns unique writers; borrowed SQL scopes never reacquire a database.
use crate::{Db, ReaderPool, Store, StoreError};
use rusqlite::Connection;
use std::{num::NonZeroUsize, sync::Arc};

pub struct LibraryConnection<'a> {
    pub(crate) library: &'a Connection,
    pub(crate) data_dir: &'a std::path::Path,
    pub(crate) authorization: Option<(domain::UserId, std::collections::HashSet<String>)>,
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
    pub(crate) fn with_reference_write<T>(
        &self,
        work: impl FnOnce(&LibraryConnection<'_>, &SubscribeConnection<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.with_writer(Db::Subscribe, |subscribe| {
            self.with_writer(Db::Library, |library| {
                work(
                    &self.library_connection(library),
                    &SubscribeConnection { subscribe },
                )
            })
        })
    }
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
    pub(crate) fn authorized_library_ids(
        &self,
        user: domain::UserId,
    ) -> Result<std::collections::HashSet<String>, StoreError> {
        self.authorization
            .as_ref()
            .filter(|(owner, _)| *owner == user)
            .map(|(_, ids)| ids.clone())
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
