//! 授权与实体范围：先算"这次请求能看哪些库、哪些媒体"，再交给谓词层。
//!
//! 授权是两层交集：`authorized_library_ids` 给出用户有权读的库，
//! `library_removals` 里未成功删除的库一律排除（隐藏库不能借分页泄露）。

use std::collections::BTreeSet;

use rusqlite::Connection;

use domain::{ProtocolItemQuery, ProtocolParent};

use crate::Store;
use crate::StoreError;

/// 请求展开的粒度：`Rows` 由哪几支构成，决定 `rows` 是每媒体一行、每单元一行，还是两者同页。
///
/// `Mixed` 只出现在 `IncludeItemTypes` 同时点名作品级类型与 `Episode` 时
/// （例如 `Series,Episode`）：剧与分集是两个不同的逻辑 ID，必须同页而不是二选一。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Granularity {
    Work,
    Unit,
    Mixed,
}

impl Granularity {
    /// 是否需要作品级（每媒体一行）的那一支。
    pub(super) fn wants_work(self) -> bool {
        matches!(self, Self::Work | Self::Mixed)
    }

    /// 是否需要分集级（每单元一行）的那一支。
    pub(super) fn wants_unit(self) -> bool {
        matches!(self, Self::Unit | Self::Mixed)
    }
}

/// 请求的实体范围与它的 SQL 形态。
pub(super) struct Scope {
    /// `scoped` 的 `WHERE` 片段，只引用别名 `i`，可直接被人物/类型解析查询复用。
    pub(super) where_sql: String,
    /// 绑定参数的起始值，顺序与 `where_sql` 里的 `?1..?n` 一致。
    pub(super) values: Vec<rusqlite::types::Value>,
    /// 本次请求要展开的粒度。
    pub(super) granularity: Granularity,
}

impl Scope {
    /// 解析范围；返回 `None` 表示这次请求不可能命中任何行（空页，不是错误）。
    pub(super) fn resolve(
        store: &Store,
        conn: &Connection,
        request: &ProtocolItemQuery,
    ) -> Result<Option<Self>, StoreError> {
        if matches!(request.parent, ProtocolParent::Unknown) {
            return Ok(None);
        }
        let blocked = blocked_libraries(conn)?;
        let library_ids = visible_libraries(store, request, &blocked)?;
        if library_ids.is_empty() {
            return Ok(None);
        }
        let mut values = Vec::new();
        let placeholders = library_ids
            .iter()
            .map(|id| {
                values.push(rusqlite::types::Value::Text(id.clone()));
                format!("?{}", values.len())
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut where_sql = format!(
            "i.library_id IN ({placeholders}) \
             AND NOT EXISTS (SELECT 1 FROM library_removals r \
               WHERE r.library_id = i.library_id AND r.state != 'succeeded')"
        );
        // 有具体挂点时把媒体也钉死：分集请求靠它命中 series，详情请求靠它只取一行。
        if let Some(media) = pinned_media(request) {
            values.push(rusqlite::types::Value::Text(media.to_string()));
            where_sql.push_str(&format!(" AND i.media_id = ?{}", values.len()));
        }
        Ok(Some(Self {
            where_sql,
            values,
            granularity: granularity(request),
        }))
    }
}

/// 用户有权读、且没有被删除流程冻结/挂起的库，按 ID 排序保证分页稳定。
fn visible_libraries(
    store: &Store,
    request: &ProtocolItemQuery,
    blocked: &BTreeSet<String>,
) -> Result<Vec<String>, StoreError> {
    let authorized = store.authorized_library_ids(request.user_id)?;
    let mut ids: Vec<String> = match &request.parent {
        ProtocolParent::Library(id) => {
            let id = id.to_string();
            if authorized.contains(&id) && !blocked.contains(&id) {
                vec![id]
            } else {
                Vec::new()
            }
        }
        _ => authorized
            .into_iter()
            .filter(|id| !blocked.contains(id))
            .collect(),
    };
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// 未成功完成的删除记录：这些库的历史行还在表里，但不许再被读到。
fn blocked_libraries(conn: &Connection) -> Result<BTreeSet<String>, StoreError> {
    let mut stmt =
        conn.prepare("SELECT library_id FROM library_removals WHERE state != 'succeeded'")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut blocked = BTreeSet::new();
    for row in rows {
        blocked.insert(row?);
    }
    Ok(blocked)
}

/// 请求钉住的媒体：`Series`/`Season` 父级与 `series_id`/`season` 参数都指向同一部作品。
fn pinned_media(request: &ProtocolItemQuery) -> Option<domain::MediaId> {
    match &request.parent {
        ProtocolParent::Series(media) => Some(*media),
        ProtocolParent::Season(media, _) => Some(*media),
        _ => request
            .series_id
            .or_else(|| request.season.map(|(media, _)| media)),
    }
}

/// 请求粒度：显式点名的类型优先；未点名类型时，钉住作品的分集请求走分集级。
///
/// 旧实现只看"含不含 `Episode`"，于是 `Series,Episode` 会丢掉剧那一半，`IsFolder=false`
/// 又和分集级的 `kind = 'tv'` 互斥，两种组合都在这里按类型分别定支修好。
pub(super) fn granularity(request: &ProtocolItemQuery) -> Granularity {
    let unit_wanted = request
        .include_types
        .contains(&domain::ProtocolItemType::Episode);
    let work_wanted = request.include_types.iter().any(|kind| {
        matches!(
            kind,
            domain::ProtocolItemType::Movie
                | domain::ProtocolItemType::Series
                | domain::ProtocolItemType::Season
                | domain::ProtocolItemType::Video
        )
    });
    match (work_wanted, unit_wanted) {
        (true, true) => Granularity::Mixed,
        (true, false) => Granularity::Work,
        (false, true) => Granularity::Unit,
        (false, false) if pinned_media(request).is_some() => Granularity::Unit,
        (false, false) => Granularity::Work,
    }
}
