use rusqlite::params;

use super::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogCacheRow {
    pub source: String,
    pub cache_key: String,
    pub body: String,
    pub fetched_at: i64,
    pub expires_at: Option<i64>,
}

pub(crate) const CACHE_TABLES: [(&str, &str); 6] = [
    ("tmdb", "tmdb_cache"),
    ("douban", "douban_cache"),
    ("tvdb", "tvdb_cache"),
    ("bangumi", "bangumi_cache"),
    ("anilist", "anilist_cache"),
    ("fanart", "fanart_cache"),
];

fn cache_table(source: &str) -> Result<&'static str, StoreError> {
    CACHE_TABLES
        .iter()
        .find(|(name, _)| *name == source)
        .map(|(_, table)| *table)
        .ok_or_else(|| StoreError::Invalid(format!("unknown cache source: {source}")))
}

impl Store {
    pub fn put_catalog_cache(
        &self,
        source: &str,
        cache_key: &str,
        body: &str,
        fetched_at: i64,
        expires_at: Option<i64>,
    ) -> Result<(), StoreError> {
        self.with_cache_write(|db| {
            db.put_catalog_cache(source, cache_key, body, fetched_at, expires_at)
        })
    }

    pub fn get_catalog_cache(
        &self,
        source: &str,
        cache_key: &str,
    ) -> Result<Option<String>, StoreError> {
        self.with_cache_read(|db| db.get_catalog_cache(source, cache_key))
    }

    pub fn list_catalog_cache(&self) -> Result<Vec<CatalogCacheRow>, StoreError> {
        self.with_cache_read(|db| db.list_catalog_cache())
    }

    pub fn delete_catalog_cache(&self, source: &str, cache_key: &str) -> Result<bool, StoreError> {
        self.with_cache_write(|db| db.delete_catalog_cache(source, cache_key))
    }
}

#[path = "catalog/connection.rs"]
mod connection;
