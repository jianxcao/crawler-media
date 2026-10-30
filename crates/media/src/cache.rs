mod backend;
pub use backend::CacheBackend;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
#[cfg(test)]
use rusqlite::Connection;
use std::sync::Arc;

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
    backend: Arc<dyn CacheBackend>,
    ttl_secs: i64,
    /// 测试用固定时钟。`None` = 每次访问都取实时时钟（生产路径），
    /// 避免「进程启动时钉死的 now 让缓存永不过期」。
    now: Mutex<Option<i64>>,
    source: crate::response_cache::CacheSource,
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
        let cache_source = source.parse::<crate::response_cache::CacheSource>()?;
        let conn = crate::response_cache::open(path)?;
        Ok(Self::with_backend(
            Arc::new(backend::OwnedSqlite(Mutex::new(conn))),
            cache_source,
            now,
        ))
    }

    pub fn with_backend(
        backend: Arc<dyn CacheBackend>,
        source: crate::response_cache::CacheSource,
        now: i64,
    ) -> Self {
        Self {
            backend,
            ttl_secs: DEFAULT_TTL_SECS,
            now: Mutex::new((now != 0).then_some(now)),
            source,
        }
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
        self.backend
            .put(self.source, key, body, now, Some(expires))?;
        Ok(())
    }

    /// Decode at the public endpoint's contract before replacing durable bytes.
    pub(crate) fn get_or_fetch<T>(
        &self,
        http: &impl crate::client::CatalogGet,
        key: &str,
        decode: impl Fn(&str) -> Result<T, TmdbError>,
    ) -> Result<T, TmdbError> {
        if let Some(body) = self.get_fresh(key)? {
            match decode(&body) {
                Ok(value) => {
                    tracing::debug!(source = %self.source, path = key, "Metadata cache hit");
                    return Ok(value);
                }
                Err(error @ TmdbError::NotFound(_)) => return Err(error),
                Err(error) => {
                    tracing::warn!(source = %self.source, path = key, %error, "Malformed fresh Metadata cache; refetching");
                }
            }
        }
        let fetched = match http.get(key) {
            Ok(body) => match decode(&body) {
                Ok(value) => {
                    self.put(key, &body)?;
                    Ok(value)
                }
                Err(error @ TmdbError::NotFound(_)) => {
                    self.put(key, &body)?;
                    return Err(error);
                }
                Err(error) => Err(error),
            },
            Err(TmdbError::NotFound(message)) => {
                let body = serde_json::json!({
                    "success": false,
                    "status_code": 34,
                    "status_message": message,
                });
                self.put(key, &body.to_string())?;
                return Err(TmdbError::NotFound(message));
            }
            Err(error) => Err(error),
        };
        match fetched {
            Ok(value) => Ok(value),
            Err(error) => {
                tracing::warn!(source = %self.source, path = key, %error, "Metadata refresh failed; trying valid stale cache");
                if let Some(body) = self.get_stale(key)? {
                    match decode(&body) {
                        Ok(value) => {
                            tracing::info!(
                                source = %self.source,
                                path = key,
                                "Using valid stale Metadata cache"
                            );
                            return Ok(value);
                        }
                        Err(stale_error) => {
                            tracing::warn!(source = %self.source, path = key, %stale_error, "Rejected malformed stale Metadata cache");
                        }
                    }
                }
                tracing::error!(source = %self.source, path = key, %error, "Metadata unavailable without valid cache");
                Err(error)
            }
        }
    }

    fn load(&self, key: &str, allow_stale: bool) -> Result<Option<String>, TmdbError> {
        let now = self.now_secs();
        let row = self.backend.get(self.source, key)?;
        let Some(row) = row else {
            return Ok(None);
        };
        if !allow_stale
            && let Some(expires) = row.expires_at
            && expires <= now
        {
            return Ok(None);
        }
        Ok(Some(row.body))
    }
}

/// 所有 `CatalogCache` 连接（含 fanart 缓存使用的同一个 `open_source`）统一的
/// **连接级**策略。`busy_timeout`/`foreign_keys` 是连接级设置，bundled 构建默认
/// 5000ms/ON，显式设置是为了不依赖构建默认值；`journal_mode=WAL` 是持久属性。
#[cfg(test)]
fn apply_connection_policy(conn: &Connection) -> Result<(), TmdbError> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", "5000")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

#[cfg(test)]
mod policy_tests {
    use super::apply_connection_policy;

    /// 连接级 pragma 必须在**我们自己打开的连接**上断言：外部重开连接只会看到
    /// bundled 构建的默认值，证明不了策略被应用。
    #[test]
    fn catalog_cache_connection_policy_sets_timeout_and_foreign_keys() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        apply_connection_policy(&conn).unwrap();
        let timeout: i64 = conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        let foreign_keys: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
        assert_eq!(foreign_keys, 1);
    }
}
