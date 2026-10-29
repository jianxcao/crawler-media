use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use super::{Store, StoreError, schema};
use std::num::NonZeroUsize;

impl Store {
    pub fn open_library_read_pool(
        &self,
        size: NonZeroUsize,
    ) -> Result<crate::LibraryReadPool, StoreError> {
        crate::library_read::LibraryReadPool::open(self.data_dir.join("library.db"), size)
    }
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
            schema::layout::CACHE_USER_VERSION_I64,
        )?;
        schema::layout::preflight_check_existing_file(
            "library",
            &library_path,
            schema::layout::LIBRARY_APPLICATION_ID,
            schema::layout::LIBRARY_USER_VERSION,
        )?;
        schema::layout::preflight_check_existing_file(
            "subscribe",
            &subscribe_path,
            schema::layout::SUBSCRIBE_APPLICATION_ID,
            schema::layout::SUBSCRIBE_USER_VERSION,
        )?;

        let app = Connection::open(&app_path)?;
        let cache = Connection::open(&cache_path)?;
        let library = Connection::open(&library_path)?;
        let subscribe = Connection::open(&subscribe_path)?;

        // Create path only: `auto_vacuum` lives in the database header and SQLite
        // only rewrites it while the file still has no tables *and* is not in WAL
        // mode. Both hold exactly here, before `apply_connection_policy` turns on
        // WAL and before `initialize_layout()` runs any DDL, so a later `VACUUM` never has to
        // pay for the change. Existing files are validated without applying DDL.
        for conn in [&app, &cache, &library, &subscribe] {
            apply_auto_vacuum_on_create(conn)?;
        }
        for (conn, kind) in [
            (&app, DbKind::Authoritative),
            (&cache, DbKind::Cache),
            (&library, DbKind::Authoritative),
            (&subscribe, DbKind::Authoritative),
        ] {
            apply_connection_policy(conn, kind)?;
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
        store.initialize_layout()?;
        Ok(store)
    }

    /// A read-only facade over the same four files (L1 read/write split).
    ///
    /// Request reads run here instead of behind the process-wide writer `Store`
    /// lock: SQLite WAL lets them read the last committed snapshot while a scan,
    /// probe or scrape batch holds the write lock. Every connection is opened
    /// `SQLITE_OPEN_READ_ONLY` + `query_only`, so a read path that accidentally
    /// writes fails loudly instead of racing the writer. Root aliases are shared per
    /// database file ([`crate::path_key`]), so `norm_path_key` agrees with the writer.
    pub fn open_reader(&self) -> Result<Self, StoreError> {
        let open = |path: &Path| -> Result<Connection, StoreError> {
            let conn = Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | rusqlite::OpenFlags::SQLITE_OPEN_URI,
            )?;
            conn.pragma_update(None, "query_only", "ON")?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            conn.busy_timeout(std::time::Duration::from_millis(5000))?;
            apply_reader_pragma_tuning(&conn)?;
            register_casefold(&conn)?;
            crate::path_key::register(&conn)?;
            Ok(conn)
        };
        Ok(Self {
            data_dir: self.data_dir.clone(),
            root_ledger_scans: std::sync::atomic::AtomicUsize::new(0),
            media_ledger_reads: std::sync::atomic::AtomicUsize::new(0),
            role_lookups: std::sync::atomic::AtomicUsize::new(0),
            app_path: self.app_path.clone(),
            app: open(&self.app_path)?,
            cache: open(&self.data_dir.join("cache.db"))?,
            library: open(&self.data_dir.join("library.db"))?,
            subscribe: open(&self.data_dir.join("subscribe.db"))?,
        })
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

    fn initialize_layout(&self) -> Result<(), StoreError> {
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
            schema::layout::CACHE_USER_VERSION_I64,
        )?;
        schema::layout::initialize_or_validate(
            "library",
            &self.library,
            schema::layout::LIBRARY_DDL,
            schema::layout::LIBRARY_APPLICATION_ID,
            schema::layout::LIBRARY_USER_VERSION,
        )?;
        schema::layout::initialize_or_validate(
            "subscribe",
            &self.subscribe,
            schema::layout::SUBSCRIBE_DDL,
            schema::layout::SUBSCRIBE_APPLICATION_ID,
            schema::layout::SUBSCRIBE_USER_VERSION,
        )?;
        schema::seed_defaults(&self.app, &self.library, &self.data_dir)?;
        crate::path_key::reload_root_aliases(&self.library)?;
        Ok(())
    }

    pub fn put_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        if key == "metadata.language" {
            return Err(StoreError::Invalid(
                "use metadata language configuration".into(),
            ));
        }
        self.app.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> Result<bool, StoreError> {
        if key == "metadata.language" {
            return Err(StoreError::Invalid(
                "use metadata language configuration".into(),
            ));
        }
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

/// 建库路径上的 `auto_vacuum`：只在**未初始化**的文件上执行，且必须早于
/// `apply_connection_policy`（它把文件切成 WAL）。SQLite 只在「没有表且不是 WAL」
/// 时改写这个文件头标志；在 WAL 库或事务里执行是**静默 no-op**，所以它不能留在
/// DDL 里，也不能靠日后的 `VACUUM` 补。
///
/// 2 = INCREMENTAL：维护任务可以不重写整库地归还空闲页（`PRAGMA incremental_vacuum`）。
fn apply_auto_vacuum_on_create(conn: &Connection) -> Result<(), StoreError> {
    if !schema::layout::is_uninitialized(conn)? {
        return Ok(());
    }
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    Ok(())
}

/// 连接属于哪个库：决定**连接级**耐久性/等待策略。
///
/// 库文件本身的性质（WAL、`auto_vacuum`）与种类无关；这里只区分「丢了要不要紧」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DbKind {
    /// 权威库：app / library / subscribe。已提交事务必须扛住掉电。
    Authoritative,
    /// cache.db：可重建的缓存目录，容忍掉电丢最近提交，换写吞吐。
    Cache,
    /// 实例租约心跳的专用 app.db 连接：写同行，但需要比前台更长的等锁预算。
    Heartbeat,
}

impl DbKind {
    /// `synchronous`：1 = NORMAL，2 = FULL。
    fn synchronous(self) -> &'static str {
        match self {
            // WAL + NORMAL 只保证「不损坏文件」，掉电可能丢最近一次提交——缓存可以重拉。
            Self::Cache => "NORMAL",
            // 权威库与租约显式 FULL，不依赖构建默认值。
            Self::Authoritative | Self::Heartbeat => "FULL",
        }
    }

    fn busy_timeout_ms(self) -> i64 {
        match self {
            Self::Heartbeat => HEARTBEAT_BUSY_TIMEOUT_MS,
            Self::Authoritative | Self::Cache => DEFAULT_BUSY_TIMEOUT_MS,
        }
    }
}

/// 页缓存：16 MiB / 连接（负值表示 KiB）。
pub(crate) const CACHE_SIZE_KIB: i64 = -16_384;
/// WAL 文件上限 64 MiB：checkpoint 后按此截断，避免 WAL 无限长。
pub(crate) const JOURNAL_SIZE_LIMIT_BYTES: i64 = 67_108_864;
/// memory-map 上限 256 MiB / 连接（地址空间，不是常驻内存；见模块文档）。
pub(crate) const MMAP_SIZE_BYTES: i64 = 268_435_456;
/// `temp_store = MEMORY`：临时表/排序走内存，不落盘。
pub(crate) const TEMP_STORE_MEMORY: i64 = 2;
/// 打开时带 hint 的 `PRAGMA optimize`：0x10000 = 有益时才 ANALYZE，低位是分析位掩码。
const OPTIMIZE_ON_OPEN: i64 = 0x10002;
const DEFAULT_BUSY_TIMEOUT_MS: i64 = 5_000;
/// 心跳连接的等锁预算：`api::instance_lease` 的心跳间隔（20 s）的一半。
pub const HEARTBEAT_BUSY_TIMEOUT_MS: i64 = 10_000;

/// 所有 Store 连接的**连接级**策略，按库区分耐久性。
///
/// `busy_timeout` 与 `foreign_keys` 是连接级设置：bundled SQLite 构建的默认值恰好是
/// 5000ms / ON，显式设置是为了不依赖构建默认值。`journal_mode=WAL` 与 `auto_vacuum`
/// 是持久属性，写在数据库文件头里（后者由 [`apply_auto_vacuum_on_create`] 在 create
/// 路径设置）。
///
/// 性能项都在这里一次性说明：
/// - `cache_size=-16384` 是**每连接** 16 MiB 页缓存，不共享；
/// - `mmap_size=256 MiB` 是**每连接**的地址空间上限，不是预留的常驻内存：只有真正
///   读到的页会进入 RSS，且这些页是干净的文件页，内核可以随时回收。代价是虚拟内存
///   尺寸按连接数叠加（Store 四连接 + 读池 + 心跳 + 维护临时连接），因此这里给的是
///   上限而不是按库大小推算的值；
/// - `journal_size_limit` 让 WAL 在 checkpoint 后截断到 64 MiB；
/// - `temp_store=MEMORY` 让临时表/排序不落盘（受 `cache_size` 之外的内存约束）。
pub(crate) fn apply_connection_policy(conn: &Connection, kind: DbKind) -> Result<(), StoreError> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", kind.busy_timeout_ms())?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", kind.synchronous())?;
    conn.pragma_update(None, "cache_size", CACHE_SIZE_KIB)?;
    conn.pragma_update(None, "temp_store", TEMP_STORE_MEMORY)?;
    conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT_BYTES)?;
    conn.pragma_update(None, "mmap_size", MMAP_SIZE_BYTES)?;
    register_casefold(conn)?;
    crate::path_key::register(conn)?;
    // 打开时的优化提示：只在 ANALYZE 确有收益的表上统计，不阻塞启动。
    conn.execute_batch(&format!("PRAGMA optimize={OPTIMIZE_ON_OPEN};"))?;
    Ok(())
}

/// 读池连接的共享调优：只读连接不需要 `journal_size_limit`，也绝不能跑写入型
/// `PRAGMA optimize`，但页缓存 / mmap / temp_store 必须与写连接一致。
pub(crate) fn apply_reader_pragma_tuning(conn: &Connection) -> Result<(), StoreError> {
    conn.pragma_update(None, "cache_size", CACHE_SIZE_KIB)?;
    conn.pragma_update(None, "temp_store", TEMP_STORE_MEMORY)?;
    conn.pragma_update(None, "mmap_size", MMAP_SIZE_BYTES)?;
    Ok(())
}

/// 一个连接的 pragma 快照（诊断与测试用）。
///
/// 这些值**不持久**：只有本进程打开的连接才看得到；外部重开连接读到的是构建默认值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionPragmas {
    pub synchronous: i64,
    pub journal_mode_wal: bool,
    pub temp_store: i64,
    pub cache_size: i64,
    pub journal_size_limit: i64,
    pub mmap_size: i64,
    pub busy_timeout_ms: i64,
    pub foreign_keys: i64,
    pub query_only: i64,
    pub auto_vacuum: i64,
}

impl ConnectionPragmas {
    fn read(conn: &Connection) -> Result<Self, StoreError> {
        let journal_mode: String =
            conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        Ok(Self {
            synchronous: pragma_i64(conn, "synchronous")?,
            journal_mode_wal: journal_mode.eq_ignore_ascii_case("wal"),
            temp_store: pragma_i64(conn, "temp_store")?,
            cache_size: pragma_i64(conn, "cache_size")?,
            journal_size_limit: pragma_i64(conn, "journal_size_limit")?,
            mmap_size: pragma_i64(conn, "mmap_size")?,
            busy_timeout_ms: pragma_i64(conn, "busy_timeout")?,
            foreign_keys: pragma_i64(conn, "foreign_keys")?,
            query_only: pragma_i64(conn, "query_only")?,
            auto_vacuum: pragma_i64(conn, "auto_vacuum")?,
        })
    }
}

fn pragma_i64(conn: &Connection, name: &str) -> Result<i64, StoreError> {
    conn.pragma_query_value(None, name, |row| row.get(0))
        .map_err(Into::into)
}

impl Store {
    /// 四个库的**连接级** pragma 快照（诊断与测试用；值不持久）。
    pub fn connection_pragmas(&self) -> Vec<(&'static str, ConnectionPragmas)> {
        [
            ("app", &self.app),
            ("cache", &self.cache),
            ("library", &self.library),
            ("subscribe", &self.subscribe),
        ]
        .into_iter()
        .filter_map(|(name, conn)| {
            ConnectionPragmas::read(conn)
                .ok()
                .map(|pragmas| (name, pragmas))
        })
        .collect()
    }
}

/// 关闭连接前跑一次 `PRAGMA optimize`（SQLite 推荐的「关闭时」用法）：
/// 用连接生命周期内的查询统计决定要不要刷新 `sqlite_stat1`。
impl Drop for Store {
    fn drop(&mut self) {
        for (name, conn) in [
            ("app", &self.app),
            ("cache", &self.cache),
            ("library", &self.library),
            ("subscribe", &self.subscribe),
        ] {
            if let Err(error) = conn.execute_batch("PRAGMA optimize;") {
                tracing::warn!(database = name, %error, "关闭连接前的 PRAGMA optimize 失败");
            }
        }
    }
}

/// `casefold(text)`: Unicode lower-casing with Rust's rules. SQLite's `lower()` only folds
/// ASCII, so protocol search/sort/filter must use this on both the column and the needle
/// (which Rust folds with `to_lowercase`) to stay symmetric (R23).
pub(crate) fn register_casefold(conn: &Connection) -> Result<(), StoreError> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function(
        "casefold",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let value = ctx.get_raw(0);
            Ok(match value {
                rusqlite::types::ValueRef::Text(bytes) => {
                    Some(String::from_utf8_lossy(bytes).to_lowercase())
                }
                rusqlite::types::ValueRef::Null => None,
                other => Some(match other {
                    rusqlite::types::ValueRef::Integer(i) => i.to_string(),
                    rusqlite::types::ValueRef::Real(r) => r.to_string(),
                    _ => String::new(),
                }),
            })
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod policy_tests {
    use std::path::PathBuf;

    use super::{CACHE_SIZE_KIB, ConnectionPragmas, DbKind, apply_connection_policy};

    /// 文件库连接：`journal_size_limit`/`mmap_size` 在 `:memory:` 上读不到行，
    /// 而策略本来就是给真实文件的。
    fn file_connection(dir: &tempfile::TempDir, name: &str) -> (PathBuf, rusqlite::Connection) {
        let path = dir.path().join(name);
        let conn = rusqlite::Connection::open(&path).unwrap();
        (path, conn)
    }

    /// 连接级 pragma 只能在**我们自己打开的连接**上断言：外部重开连接只会看到
    /// bundled 构建的默认值，证明不了策略被应用。
    #[test]
    fn connection_policy_sets_timeout_and_foreign_keys() {
        let dir = tempfile::tempdir().unwrap();
        let (_path, conn) = file_connection(&dir, "app.db");
        apply_connection_policy(&conn, DbKind::Authoritative).unwrap();
        let pragmas = ConnectionPragmas::read(&conn).unwrap();
        assert_eq!(pragmas.busy_timeout_ms, 5_000);
        assert_eq!(pragmas.foreign_keys, 1);
        assert_eq!(pragmas.cache_size, CACHE_SIZE_KIB);
        assert!(pragmas.journal_mode_wal);
    }

    /// 权威库耐久性不依赖构建默认值：即便连接先被降级，策略也必须把它拉回 FULL。
    #[test]
    fn connection_policy_forces_synchronous_full() {
        let dir = tempfile::tempdir().unwrap();
        let (_path, conn) = file_connection(&dir, "app.db");
        conn.pragma_update(None, "synchronous", "OFF").unwrap();
        apply_connection_policy(&conn, DbKind::Authoritative).unwrap();
        let sync: i64 = conn
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();
        assert_eq!(sync, 2, "SQLite FULL");
    }

    /// 缓存库是唯一允许 NORMAL 的库；心跳连接保持 FULL 但要求更长的等锁预算。
    #[test]
    fn connection_policy_splits_durability_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        let (_path, cache) = file_connection(&dir, "cache.db");
        apply_connection_policy(&cache, DbKind::Cache).unwrap();
        let cache = ConnectionPragmas::read(&cache).unwrap();
        assert_eq!(cache.synchronous, 1, "cache.db 用 NORMAL");
        assert_eq!(cache.busy_timeout_ms, 5_000);

        let (_path, heartbeat) = file_connection(&dir, "heartbeat.db");
        apply_connection_policy(&heartbeat, DbKind::Heartbeat).unwrap();
        let heartbeat = ConnectionPragmas::read(&heartbeat).unwrap();
        assert_eq!(heartbeat.synchronous, 2, "租约行仍要 FULL");
        assert_eq!(heartbeat.busy_timeout_ms, 10_000, "心跳等锁预算更长");
        assert_eq!(heartbeat.temp_store, 2, "MEMORY");
        assert_eq!(heartbeat.mmap_size, 268_435_456);
    }
}
