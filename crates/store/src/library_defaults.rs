use crate::StoreError;
use rusqlite::params;

/// 在数据库连接中强制维护每个媒体库类型的默认库不变量：
/// 1. 若某个类型存在媒体库但没有默认库，自动将排在最前的库提升为默认库；
/// 2. 若某个类型意外存在多个默认库，保留排在最前的为默认库，其余降级；
/// 3. 将悬空的 library_roots (library_id IS NULL) 自动关联到该类型的默认库。
pub fn ensure_library_defaults(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT kind FROM libraries
         UNION
         SELECT DISTINCT kind FROM library_roots",
    )?;
    let kinds: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    for kind in kinds {
        ensure_kind_default(conn, &kind)?;
    }
    Ok(())
}

fn ensure_kind_default(conn: &rusqlite::Connection, kind: &str) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "SELECT id FROM libraries WHERE kind = ?1 AND is_default = 1 ORDER BY sort_order ASC, id ASC",
    )?;
    let defaults: Vec<String> = stmt
        .query_map(params![kind], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    let default_id = if let Some(first) = defaults.first() {
        for extra in &defaults[1..] {
            conn.execute(
                "UPDATE libraries SET is_default = 0 WHERE id = ?1",
                params![extra],
            )?;
        }
        first.clone()
    } else {
        let candidate: Option<String> = conn
            .query_row(
                "SELECT id FROM libraries WHERE kind = ?1 ORDER BY sort_order ASC, id ASC LIMIT 1",
                params![kind],
                |row| row.get(0),
            )
            .ok();
        if let Some(cand_id) = candidate {
            conn.execute(
                "UPDATE libraries SET is_default = 1 WHERE id = ?1",
                params![cand_id],
            )?;
            cand_id
        } else {
            return Ok(());
        }
    };

    conn.execute(
        "UPDATE library_roots SET library_id = ?1 WHERE kind = ?2 AND library_id IS NULL",
        params![default_id, kind],
    )?;
    Ok(())
}
