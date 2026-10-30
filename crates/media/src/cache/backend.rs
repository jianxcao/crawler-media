//! Storage injection keeps application clients on their owner's database handles.
use parking_lot::Mutex;
use rusqlite::Connection;

use crate::TmdbError;
use crate::response_cache::{CacheRow, CacheSource};

pub trait CacheBackend: Send + Sync {
    fn get(&self, source: CacheSource, key: &str) -> Result<Option<CacheRow>, TmdbError>;
    fn put(
        &self,
        source: CacheSource,
        key: &str,
        body: &str,
        fetched_at: i64,
        expires_at: Option<i64>,
    ) -> Result<(), TmdbError>;
}

pub(super) struct OwnedSqlite(pub(super) Mutex<Connection>);

impl CacheBackend for OwnedSqlite {
    fn get(&self, source: CacheSource, key: &str) -> Result<Option<CacheRow>, TmdbError> {
        Ok(crate::response_cache::get(&self.0.lock(), source, key)?)
    }

    fn put(
        &self,
        source: CacheSource,
        key: &str,
        body: &str,
        fetched_at: i64,
        expires_at: Option<i64>,
    ) -> Result<(), TmdbError> {
        Ok(crate::response_cache::put(
            &self.0.lock(),
            source,
            key,
            body,
            fetched_at,
            expires_at,
        )?)
    }
}
