//! 授权与实体范围：先算"这次请求能看哪些库、哪些媒体"，再交给谓词层。
//!
//! 授权是两层交集：`authorized_library_ids` 给出用户有权读的库，
//! `library_removals` 里未成功删除的库一律排除（隐藏库不能借分页泄露）。

use std::collections::{BTreeSet, HashSet};

use rusqlite::{Connection, OptionalExtension};

use domain::{LibraryId, ProtocolItemQuery, ProtocolParent};

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
    /// 实例默认元数据语言的占位符（`?n`，值可为 NULL）：locale 链
    /// Library → 实例 在 SQL 里解析（R23），不再按字典序取首个语言。
    pub(super) instance_locale: String,
    pub(super) library_ids: Vec<String>,
    pub(super) pinned: bool,
    pub(super) pinned_media: bool,
}

impl Scope {
    /// 解析范围；返回 `None` 表示这次请求不可能命中任何行（空页，不是错误）。
    pub(super) fn resolve(
        conn: &Connection,
        authorized: &HashSet<LibraryId>,
        request: &ProtocolItemQuery,
    ) -> Result<Option<Self>, StoreError> {
        if matches!(request.parent, ProtocolParent::Unknown) {
            return Ok(None);
        }
        let blocked = blocked_libraries(conn)?;
        let mut library_ids = visible_libraries(conn, authorized, request, &blocked)?;
        if let Some(media) = pinned_media(request) {
            let mut owners = conn.prepare("SELECT library_id FROM library_files WHERE media_id=?1 UNION SELECT library_id FROM library_items WHERE media_id=?1")?;
            let owners = owners
                .query_map([media.to_string()], |r| r.get::<_, String>(0))?
                .collect::<Result<BTreeSet<_>, _>>()?;
            library_ids.retain(|id| owners.contains(id));
        }
        if let ProtocolParent::Folder { root_id } = &request.parent {
            let owner: Option<String> = conn
                .query_row(
                    "SELECT library_id FROM library_roots WHERE root_id=?1",
                    [root_id],
                    |r| r.get(0),
                )
                .optional()?;
            library_ids.retain(|id| Some(id) == owner.as_ref());
        }
        for id in &library_ids {
            let state: Option<String> = conn
                .query_row(
                    "SELECT lifecycle_state FROM libraries WHERE library_id=?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            if state.as_deref() == Some("reindexing") {
                return Err(StoreError::QueryUnavailable {
                    library_id: id.parse()?,
                });
            }
        }
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
        values.push(match crate::metadata_language::default_locale(conn)? {
            Some(locale) => rusqlite::types::Value::Text(locale),
            None => rusqlite::types::Value::Null,
        });
        let instance_locale = format!("?{}", values.len());
        Ok(Some(Self {
            where_sql,
            values,
            granularity: granularity(request),
            instance_locale,
            library_ids,
            pinned_media: pinned_media(request).is_some(),
            pinned: pinned_media(request).is_some()
                || matches!(request.parent, ProtocolParent::Folder { .. }),
        }))
    }
}

/// 用户有权读、且没有被删除流程冻结/挂起的库，按 ID 排序保证分页稳定。
fn visible_libraries(
    conn: &Connection,
    authorized: &HashSet<LibraryId>,
    request: &ProtocolItemQuery,
    blocked: &BTreeSet<String>,
) -> Result<Vec<String>, StoreError> {
    let authorized: HashSet<String> = authorized.iter().map(ToString::to_string).collect();
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
    let mut statement = conn.prepare("SELECT library_id, kind FROM libraries")?;
    let kinds = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<std::collections::HashMap<_, _>, _>>()?;
    ids.retain(|id| {
        kinds
            .get(id)
            .is_some_and(|kind| requested_kind(request, kind))
    });
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

/// Remove impossible Libraries before touching their item rows. The two branches
/// use the same kind/folder rules as predicates::kind_clauses, including mixed pages.
fn requested_kind(request: &ProtocolItemQuery, kind: &str) -> bool {
    use domain::ProtocolItemType as Type;
    let granularity = granularity(request);
    let unit = granularity.wants_unit() && kind == "tv" && request.is_folder != Some(true);
    let folder = match request.is_folder {
        Some(true) => kind == "tv",
        Some(false) => kind != "tv",
        None => true,
    };
    let explicit = request.include_types.is_empty()
        || request.include_types.iter().any(|ty| {
            matches!(
                (ty, kind),
                (Type::Movie, "movie")
                    | (Type::Series | Type::Season, "tv")
                    | (Type::Video, "video")
            )
        });
    unit || (granularity.wants_work() && folder && explicit)
}
