//! App authorization reads shared by Store and its readers.
use crate::StoreError;
use domain::{User, UserId, UserRole};
use rusqlite::{Connection, OptionalExtension, params};

/// token → 用户：会话 TTL、CLI token 例外、`enabled` 过滤全在这一条 SQL 里。
///
/// `Store::user_by_token` 共用，避免读写两条路径
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
