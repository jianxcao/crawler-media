use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use super::{Store, StoreError, schema};

impl Store {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&data_dir)?;
        Self::open_files(&data_dir, data_dir.join("app.db"))
    }

    pub fn open_at(
        data_dir: impl AsRef<Path>,
        sqlite_path: impl AsRef<Path>,
    ) -> Result<Self, StoreError> {
        Self::open_files(data_dir, sqlite_path)
    }

    fn open_files(
        data_dir: impl AsRef<Path>,
        app_path: impl AsRef<Path>,
    ) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&data_dir)?;
        let app_path = app_path.as_ref().to_path_buf();
        if let Some(parent) = app_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let cache_path = data_dir.join("cache.db");
        let library_path = data_dir.join("library.db");
        let subscribe_path = data_dir.join("subscribe.db");

        // Read-only preflight check
        schema::layout::preflight_check_existing_file(
            "app",
            &app_path,
            schema::layout::APP_APPLICATION_ID,
            schema::layout::V1_USER_VERSION,
        )?;
        schema::layout::preflight_check_existing_file(
            "cache",
            &cache_path,
            schema::layout::CACHE_APPLICATION_ID,
            schema::layout::V1_USER_VERSION,
        )?;
        schema::layout::preflight_check_existing_file(
            "library",
            &library_path,
            schema::layout::LIBRARY_APPLICATION_ID,
            schema::layout::V1_USER_VERSION,
        )?;
        schema::layout::preflight_check_existing_file(
            "subscribe",
            &subscribe_path,
            schema::layout::SUBSCRIBE_APPLICATION_ID,
            schema::layout::V1_USER_VERSION,
        )?;

        let app = Connection::open(&app_path)?;
        let cache = Connection::open(&cache_path)?;
        let library = Connection::open(&library_path)?;
        let subscribe = Connection::open(&subscribe_path)?;

        for conn in [&app, &cache, &library, &subscribe] {
            apply_connection_policy(conn)?;
        }

        let store = Self {
            data_dir,
            root_ledger_scans: std::sync::atomic::AtomicUsize::new(0),
            media_ledger_reads: std::sync::atomic::AtomicUsize::new(0),
            role_lookups: std::sync::atomic::AtomicUsize::new(0),
            app_path: app_path.clone(),
            app,
            cache,
            library,
            subscribe,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// 媒体库专属数据目录（data/library/covers 等）
    pub fn library_covers_dir(&self) -> PathBuf {
        self.data_dir.join("library").join("covers")
    }

    pub fn sqlite_path(&self) -> &Path {
        &self.app_path
    }

    /// app 库的**真实**路径。`open_at` 允许自定义，因此维护任务不能假定
    /// `data_dir/app.db`（DB8 第 5.2 节的 allowlist 由内部事实产生）。
    pub fn app_db_path(&self) -> &Path {
        &self.app_path
    }

    pub fn db_paths(&self) -> Vec<PathBuf> {
        vec![
            self.app_path.clone(),
            self.data_dir.join("cache.db"),
            self.data_dir.join("library.db"),
            self.data_dir.join("subscribe.db"),
        ]
    }

    /// Schema versions for each database file (app, catalog, library, subscribe).
    pub fn schema_versions(&self) -> Vec<(&'static str, i64)> {
        [
            ("app", &self.app),
            ("cache", &self.cache),
            ("library", &self.library),
            ("subscribe", &self.subscribe),
        ]
        .into_iter()
        .map(|(name, conn)| {
            let version = schema::layout::read_user_version(conn).unwrap_or(0);
            (name, version)
        })
        .collect()
    }

    fn migrate(&self) -> Result<(), StoreError> {
        schema::layout::initialize_or_validate(
            "app",
            &self.app,
            schema::layout::APP_DDL,
            schema::layout::APP_APPLICATION_ID,
            schema::layout::V1_USER_VERSION,
        )?;
        schema::layout::initialize_or_validate(
            "cache",
            &self.cache,
            schema::layout::CACHE_DDL,
            schema::layout::CACHE_APPLICATION_ID,
            schema::layout::V1_USER_VERSION,
        )?;
        schema::layout::initialize_or_validate(
            "library",
            &self.library,
            schema::layout::LIBRARY_DDL,
            schema::layout::LIBRARY_APPLICATION_ID,
            schema::layout::V1_USER_VERSION,
        )?;
        schema::layout::initialize_or_validate(
            "subscribe",
            &self.subscribe,
            schema::layout::SUBSCRIBE_DDL,
            schema::layout::SUBSCRIBE_APPLICATION_ID,
            schema::layout::V1_USER_VERSION,
        )?;
        schema::seed_defaults(&self.app, &self.library, &self.data_dir)?;
        Ok(())
    }

    pub fn put_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.app.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> Result<bool, StoreError> {
        let changed = self
            .app
            .execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(changed > 0)
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        self.app
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// 保存一次搜索的结果快照（按 vertical 独立，保留最近 50 条）。
    pub fn insert_search_snapshot(
        &self,
        id: &str,
        vertical: &str,
        query: &str,
        payload: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        self.app.execute(
            "INSERT INTO search_snapshots (id, vertical, query, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, vertical, query, payload, now],
        )?;
        let owner_pattern = id
            .split_once(':')
            .map(|(owner, _)| format!("{owner}:%"))
            .unwrap_or_else(|| id.to_string());
        self.app.execute(
            "DELETE FROM search_snapshots
             WHERE vertical = ?1 AND id LIKE ?2 AND id NOT IN (
                SELECT id FROM search_snapshots WHERE vertical = ?1 AND id LIKE ?2
                ORDER BY created_at DESC LIMIT 50
             )",
            params![vertical, owner_pattern],
        )?;
        Ok(())
    }

    /// `(vertical, query, payload, created_at)`。
    pub fn get_search_snapshot(
        &self,
        id: &str,
    ) -> Result<Option<(String, String, String, i64)>, StoreError> {
        self.app
            .query_row(
                "SELECT vertical, query, payload, created_at FROM search_snapshots WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn delete_search_snapshot(&self, id: &str) -> Result<bool, StoreError> {
        let changed = self
            .app
            .execute("DELETE FROM search_snapshots WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    pub fn clear_search_snapshots(&self) -> Result<(), StoreError> {
        self.app.execute("DELETE FROM search_snapshots", [])?;
        Ok(())
    }
}

/// 所有 Store 连接统一的**连接级**策略。
///
/// `busy_timeout` 与 `foreign_keys` 是连接级设置：bundled SQLite 构建的默认值
/// 恰好是 5000ms / ON，显式设置是为了不依赖构建默认值。`journal_mode=WAL`
/// 是持久属性，写在数据库文件头里。
fn apply_connection_policy(conn: &Connection) -> Result<(), StoreError> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", "5000")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

#[cfg(test)]
mod policy_tests {
    use super::apply_connection_policy;

    /// 连接级 pragma 只能在**我们自己打开的连接**上断言：外部重开连接只会看到
    /// bundled 构建的默认值，证明不了策略被应用。
    #[test]
    fn connection_policy_sets_timeout_and_foreign_keys() {
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
