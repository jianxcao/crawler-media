use rusqlite::{Connection, params};

use super::{CacheDbError, CacheSource};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheRow {
    pub source: CacheSource,
    pub entry_id: i64,
    pub cache_key: String,
    pub body: String,
    pub fetched_at: i64,
    pub expires_at: Option<i64>,
}

pub fn get(
    conn: &Connection,
    source: CacheSource,
    key: &str,
) -> Result<Option<CacheRow>, CacheDbError> {
    let table = source.table_name();
    let sql = format!(
        "SELECT entry_id, cache_key, body, fetched_at, expires_at FROM {table} WHERE cache_key = ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![key])?;
    if let Some(row) = rows.next()? {
        Ok(Some(CacheRow {
            source,
            entry_id: row.get(0)?,
            cache_key: row.get(1)?,
            body: row.get(2)?,
            fetched_at: row.get(3)?,
            expires_at: row.get(4)?,
        }))
    } else {
        Ok(None)
    }
}

pub fn put(
    conn: &Connection,
    source: CacheSource,
    key: &str,
    body: &str,
    fetched_at: i64,
    expires_at: Option<i64>,
) -> Result<(), CacheDbError> {
    let table = source.table_name();
    let sql = format!(
        "INSERT INTO {table} (cache_key, body, fetched_at, expires_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(cache_key) DO UPDATE SET
             body = excluded.body,
             fetched_at = excluded.fetched_at,
             expires_at = excluded.expires_at"
    );
    conn.execute(&sql, params![key, body, fetched_at, expires_at])?;
    Ok(())
}

pub fn delete(conn: &Connection, source: CacheSource, key: &str) -> Result<bool, CacheDbError> {
    let table = source.table_name();
    let sql = format!("DELETE FROM {table} WHERE cache_key = ?1");
    let n = conn.execute(&sql, params![key])?;
    Ok(n > 0)
}

pub fn list(conn: &Connection, source: Option<CacheSource>) -> Result<Vec<CacheRow>, CacheDbError> {
    let mut rows = Vec::new();
    match source {
        Some(s) => {
            let table = s.table_name();
            let sql = format!(
                "SELECT entry_id, cache_key, body, fetched_at, expires_at
                 FROM {table}
                 ORDER BY fetched_at DESC, entry_id ASC"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut query_rows = stmt.query([])?;
            while let Some(row) = query_rows.next()? {
                rows.push(CacheRow {
                    source: s,
                    entry_id: row.get(0)?,
                    cache_key: row.get(1)?,
                    body: row.get(2)?,
                    fetched_at: row.get(3)?,
                    expires_at: row.get(4)?,
                });
            }
        }
        None => {
            let sql = "
                SELECT 'tmdb' AS source, entry_id, cache_key, body, fetched_at, expires_at FROM tmdb_cache
                UNION ALL
                SELECT 'douban' AS source, entry_id, cache_key, body, fetched_at, expires_at FROM douban_cache
                UNION ALL
                SELECT 'tvdb' AS source, entry_id, cache_key, body, fetched_at, expires_at FROM tvdb_cache
                UNION ALL
                SELECT 'bangumi' AS source, entry_id, cache_key, body, fetched_at, expires_at FROM bangumi_cache
                UNION ALL
                SELECT 'anilist' AS source, entry_id, cache_key, body, fetched_at, expires_at FROM anilist_cache
                UNION ALL
                SELECT 'fanart' AS source, entry_id, cache_key, body, fetched_at, expires_at FROM fanart_cache
                ORDER BY fetched_at DESC, entry_id ASC
            ";
            let mut stmt = conn.prepare(sql)?;
            let mut query_rows = stmt.query([])?;
            while let Some(row) = query_rows.next()? {
                let s_str: String = row.get(0)?;
                let s = s_str.parse::<CacheSource>()?;
                rows.push(CacheRow {
                    source: s,
                    entry_id: row.get(1)?,
                    cache_key: row.get(2)?,
                    body: row.get(3)?,
                    fetched_at: row.get(4)?,
                    expires_at: row.get(5)?,
                });
            }
        }
    }
    Ok(rows)
}

pub fn details_for_prefixes(
    conn: &Connection,
    source: CacheSource,
    prefixes: &[String],
) -> Result<Vec<CacheRow>, CacheDbError> {
    let mut details = Vec::new();
    let table = source.table_name();
    for prefix in prefixes {
        let lower = format!("{prefix}?");
        let upper = format!("{prefix}@");
        let sql = format!(
            "SELECT entry_id, cache_key, body, fetched_at, expires_at
             FROM {table}
             WHERE cache_key = ?1 OR (cache_key >= ?2 AND cache_key < ?3)
             ORDER BY fetched_at DESC, entry_id ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(params![prefix, lower, upper])?;
        while let Some(row) = rows.next()? {
            details.push(CacheRow {
                source,
                entry_id: row.get(0)?,
                cache_key: row.get(1)?,
                body: row.get(2)?,
                fetched_at: row.get(3)?,
                expires_at: row.get(4)?,
            });
        }
    }
    Ok(details)
}
