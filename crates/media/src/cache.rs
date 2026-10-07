use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};

use crate::client::TmdbError;

pub const DEFAULT_TTL_SECS: i64 = 7 * 24 * 60 * 60;

/// Real unix seconds for cache expiry. `new_at(..., 0)` stays available for
/// tests; production `new()` must not use 0 or cache never expires.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

pub struct CatalogCache {
    conn: Mutex<Connection>,
    ttl_secs: i64,
    /// 测试用固定时钟。`None` = 每次访问都取实时时钟（生产路径），
    /// 避免「进程启动时钉死的 now 让缓存永不过期」。
    now: Mutex<Option<i64>>,
    source: &'static str,
}

impl CatalogCache {
    pub fn open(path: impl AsRef<Path>, now: i64) -> Result<Self, TmdbError> {
        Self::open_source(path, now, "tmdb")
    }

    pub fn open_source(
        path: impl AsRef<Path>,
        now: i64,
        source: &'static str,
    ) -> Result<Self, TmdbError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS catalog_cache (
                source TEXT NOT NULL,
                cache_key TEXT NOT NULL,
                body TEXT NOT NULL,
                fetched_at INTEGER NOT NULL,
                expires_at INTEGER,
                PRIMARY KEY (source, cache_key)
            );
            "#,
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            ttl_secs: DEFAULT_TTL_SECS,
            // now == 0 → 实时时钟（生产）；非 0 → 测试钉住的时钟。
            now: Mutex::new((now != 0).then_some(now)),
            source,
        })
    }

    /// 测试用：把时钟钉到指定值（生产不要调用，0 表示实时）。
    pub fn set_now(&self, now: i64) {
        *self.now.lock() = Some(now);
    }

    fn now_secs(&self) -> i64 {
        self.now.lock().unwrap_or_else(crate::cache::unix_now)
    }

    pub fn get_fresh(&self, key: &str) -> Result<Option<String>, TmdbError> {
        self.load(key, false)
    }

    pub fn get_stale(&self, key: &str) -> Result<Option<String>, TmdbError> {
        self.load(key, true)
    }

    pub fn put(&self, key: &str, body: &str) -> Result<(), TmdbError> {
        let now = self.now_secs();
        let expires = now + self.ttl_secs;
        self.conn.lock().execute(
            "INSERT OR REPLACE INTO catalog_cache (source, cache_key, body, fetched_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![self.source, key, body, now, expires],
        )?;
        Ok(())
    }

    fn load(&self, key: &str, allow_stale: bool) -> Result<Option<String>, TmdbError> {
        let now = self.now_secs();
        let conn = self.conn.lock();
        let row: Option<(String, Option<i64>)> = conn
            .query_row(
                "SELECT body, expires_at FROM catalog_cache WHERE source = ?1 AND cache_key = ?2",
                params![self.source, key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((body, expires_at)) = row else {
            return Ok(None);
        };
        if !allow_stale {
            if let Some(expires) = expires_at {
                if expires <= now {
                    return Ok(None);
                }
            }
        }
        Ok(Some(body))
    }
}
