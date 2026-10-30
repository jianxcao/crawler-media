//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::connections::CacheConnection<'_> {
    pub fn put_catalog_cache(
        &self,
        source: &str,
        cache_key: &str,
        body: &str,
        fetched_at: i64,
        expires_at: Option<i64>,
    ) -> Result<(), StoreError> {
        let table = cache_table(source)?;
        let sql = format!(
            "INSERT INTO {table} (cache_key, body, fetched_at, expires_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(cache_key) DO UPDATE SET
                 body = excluded.body,
                 fetched_at = excluded.fetched_at,
                 expires_at = excluded.expires_at"
        );
        self.cache
            .execute(&sql, params![cache_key, body, fetched_at, expires_at])?;
        Ok(())
    }

    pub fn get_catalog_cache(
        &self,
        source: &str,
        cache_key: &str,
    ) -> Result<Option<String>, StoreError> {
        let table = cache_table(source)?;
        let sql = format!("SELECT body FROM {table} WHERE cache_key = ?1");
        let mut stmt = self.cache.prepare(&sql)?;
        let mut rows = stmt.query(params![cache_key])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub fn list_catalog_cache(&self) -> Result<Vec<CatalogCacheRow>, StoreError> {
        let sql = "
            SELECT 'tmdb' AS source, cache_key, body, fetched_at, expires_at, entry_id FROM tmdb_cache
            UNION ALL
            SELECT 'douban' AS source, cache_key, body, fetched_at, expires_at, entry_id FROM douban_cache
            UNION ALL
            SELECT 'tvdb' AS source, cache_key, body, fetched_at, expires_at, entry_id FROM tvdb_cache
            UNION ALL
            SELECT 'bangumi' AS source, cache_key, body, fetched_at, expires_at, entry_id FROM bangumi_cache
            UNION ALL
            SELECT 'anilist' AS source, cache_key, body, fetched_at, expires_at, entry_id FROM anilist_cache
            UNION ALL
            SELECT 'fanart' AS source, cache_key, body, fetched_at, expires_at, entry_id FROM fanart_cache
            ORDER BY fetched_at DESC, entry_id ASC
        ";
        let mut stmt = self.cache.prepare(sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(CatalogCacheRow {
                source: row.get(0)?,
                cache_key: row.get(1)?,
                body: row.get(2)?,
                fetched_at: row.get(3)?,
                expires_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn delete_catalog_cache(&self, source: &str, cache_key: &str) -> Result<bool, StoreError> {
        let table = cache_table(source)?;
        let sql = format!("DELETE FROM {table} WHERE cache_key = ?1");
        let n = self.cache.execute(&sql, params![cache_key])?;
        Ok(n > 0)
    }
}
