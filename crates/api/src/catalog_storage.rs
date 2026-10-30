//! Catalog clients borrow the application's Cache writer and readonly pool.
use std::sync::Arc;

use media::cache::CacheBackend;
use media::response_cache::{CacheRow, CacheSource};

pub(crate) struct StoreCache(pub(crate) Arc<store::Store>);

impl CacheBackend for StoreCache {
    fn get(&self, source: CacheSource, key: &str) -> Result<Option<CacheRow>, media::TmdbError> {
        self.0
            .with_read(store::Db::Cache, |connection| {
                media::response_cache::get(connection, source, key)
                    .map_err(|error| store::StoreError::Invalid(error.to_string()))
            })
            .map_err(|error| media::TmdbError::CacheBackend(error.to_string()))
    }

    fn put(
        &self,
        source: CacheSource,
        key: &str,
        body: &str,
        fetched_at: i64,
        expires_at: Option<i64>,
    ) -> Result<(), media::TmdbError> {
        self.0
            .put_catalog_cache(source.as_str(), key, body, fetched_at, expires_at)
            .map_err(|error| media::TmdbError::CacheBackend(error.to_string()))
    }
}
