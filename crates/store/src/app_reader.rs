//! Task 27: app.db 的专用**读**连接，只服务身份这一跳（token→用户、用户→角色、用户详情）。
//!
//! 为什么需要它：token→user 是 app.db 的一次纯读，但 API 只能经由进程级
//! `Arc<Mutex<Store>>` 表达，于是读与写被同一把锁串起来。一次一万集的扫描每批都要
//! 拿锁写库，parking_lot 的等待者被连续重取的写者饿住超过 `AUTH_STORE_DEADLINE`
//! （1 s），`authenticate` 就判繁忙——**扫描期间每个成员请求都 503**，心跳也不例外。
//! 那是把执行器停顿换成了功能停摆，不是修复。
//!
//! app.db 是 WAL：读者读的是最后一次提交的快照，本来就不该等写者。这条连接因此
//! 完全不碰 `Store` 锁（与 Task 6 的 [`crate::AppHeartbeat`] 同一形状），只在拿不到
//! 一致快照时（WAL checkpoint / 短暂 busy）按 `busy_timeout` 退让，让上层翻译成
//! 503 `runtime.busy`——那才是这个错误码该覆盖的情形。
//!
//! 可见性：登录在返回 token **之前**已经提交（`verify_user_password` 自己一个写事务），
//! 所以下一个请求的读连接一定读得到；不存在「必须参与写者事务才能看到新 token」的
//! 情形。因此这里既不需要缓存，也不需要与写连接共享事务。
//!
//! 只读：连接以读写句柄打开（只读句柄在 WAL 下要求 `-shm` 已存在，启动早于写者时
//! 会打不开），但立刻 `PRAGMA query_only=ON`，任何写语句都会被 SQLite 拒绝——这个类型
//! 只能读，写操作仍然只走 `Store`。

use std::path::Path;
use std::sync::Mutex;

use domain::{User, UserId, UserRole};
use rusqlite::{Connection, OptionalExtension, params};

use super::{Store, StoreError};

/// 读连接自己的等锁预算（毫秒）：短于 API 层的 `AUTH_READ_DEADLINE`（1 s），
/// 让连接先放弃，由上层把「暂时读不到一致快照」翻成 503 + `Retry-After`，
/// 而不是把请求拖到超时。
pub const READER_BUSY_TIMEOUT_MS: i64 = 500;

/// 身份读取专用的 app.db 只读连接。
///
/// 只暴露读操作，且每条语句都是单行键查（走 PK / 唯一索引）：它的存在理由是
/// **不参与**进程级 `Store` 锁的排队，不是当第二个通用入口。
pub struct AppReader {
    conn: Mutex<Connection>,
}

impl AppReader {
    fn open(app_path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(app_path)?;
        // 页缓存 / mmap / temp_store 与写连接一致；刻意不跑写型 `PRAGMA optimize`
        // （读连接上它既无意义，也会被 query_only 拒绝）。
        super::open::apply_reader_pragma_tuning(&conn)?;
        conn.pragma_update(None, "busy_timeout", READER_BUSY_TIMEOUT_MS)?;
        conn.pragma_update(None, "query_only", true)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.conn
            .lock()
            .map_err(|_| StoreError::Invalid("app reader connection poisoned".into()))
    }

    /// 会话 token → 用户。会话 TTL、CLI token 例外、`enabled` 过滤与
    /// [`Store::user_by_token`] 是同一条 SQL（见 [`user_by_token_on`]）。
    pub fn user_by_token(&self, token: &str) -> Result<Option<User>, StoreError> {
        let conn = self.conn()?;
        user_by_token_on(&conn, token, super::unix_now())
    }

    pub fn user_id_by_token(&self, token: &str) -> Result<Option<UserId>, StoreError> {
        Ok(self.user_by_token(token)?.map(|user| user.id))
    }

    /// 用户角色（`require_admin` 用）；未知用户按 `member` 处理，与 Store 同口径。
    pub fn user_role(&self, id: UserId) -> Result<String, StoreError> {
        let conn = self.conn()?;
        user_role_on(&conn, id)
    }

    /// 单用户详情（`GET /auth/me` 用）。
    pub fn get_user(&self, id: UserId) -> Result<Option<User>, StoreError> {
        let conn = self.conn()?;
        get_user_on(&conn, id)
    }
}

impl Store {
    /// 为本 store 的 app.db 开一条身份读连接。
    ///
    /// 与 [`Store::open_app_heartbeat`] 同一个形状：路径取自 store 自己（`open_at`
    /// 可以给非默认 app.db 路径），不假设 `<data_dir>/app.db`。
    pub fn open_app_reader(&self) -> Result<AppReader, StoreError> {
        AppReader::open(&self.app_path)
    }
}

/// token → 用户：会话 TTL、CLI token 例外、`enabled` 过滤全在这一条 SQL 里。
///
/// `Store::user_by_token` 与 [`AppReader::user_by_token`] 共用，避免读写两条路径
/// 的鉴权口径漂移（少一处就是一处越权）。
pub(crate) fn user_by_token_on(
    conn: &Connection,
    token: &str,
    now_unix: i64,
) -> Result<Option<User>, StoreError> {
    let current_cli: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![super::users::SETTING_CLI_TOKEN],
            |row| row.get(0),
        )
        .optional()?;
    let is_cli = current_cli.as_deref() == Some(token);
    let min_created = now_unix - super::users::SESSION_TTL_SECS;
    conn.query_row(
        "SELECT u.id, u.login, u.enabled, COALESCE(u.role, 'member') FROM users u
         JOIN user_tokens t ON t.user_id = u.id
         WHERE t.token = ?1 AND (t.created_at >= ?2 OR (?3 = 1 AND u.role = 'admin')) AND u.enabled = 1",
        params![token, min_created, if is_cli { 1 } else { 0 }],
        super::users::map_user,
    )
    .optional()
    .map_err(Into::into)
}

/// 单用户角色；未知用户按 `member`（与 `Store::user_role` 同口径）。
pub(crate) fn user_role_on(conn: &Connection, id: UserId) -> Result<String, StoreError> {
    Ok(conn
        .query_row(
            "SELECT role FROM users WHERE id = ?1",
            params![id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_else(|| UserRole::Member.as_str().into()))
}

/// 单用户详情。
pub(crate) fn get_user_on(conn: &Connection, id: UserId) -> Result<Option<User>, StoreError> {
    conn.query_row(
        "SELECT id, login, enabled, COALESCE(role, 'member') FROM users WHERE id = ?1",
        params![id.to_string()],
        super::users::map_user,
    )
    .optional()
    .map_err(Into::into)
}
