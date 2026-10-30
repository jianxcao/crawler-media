use std::str::FromStr;

use super::ITEM_COLUMNS;
use super::cursor::{self, CursorKey, PageCursor, malformed_cursor};
use super::query::{Bindings, ItemQuerySql};
use crate::{
    Store, StoreError,
    library_query::types::{BrowseOrder, BrowsePage, ItemPage, LibraryItem, LibraryQuery},
};
use domain::{LibraryId, MediaId, MediaKind};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, Row, params_from_iter};

pub(crate) const MAX_ITEM_PAGE: usize = 500;
pub(crate) const ITEM_KEY_OFFSET: usize = 23;

#[derive(Clone, Copy)]
pub(crate) struct OrderTerm {
    pub(crate) expr: &'static str,
    pub(crate) descending: bool,
    pub(crate) kind: KeyKind,
}

#[derive(Clone, Copy)]
pub(crate) enum KeyKind {
    Int,
    Real,
    Text,
}

/// 一个首字母桶：档名、条目数、以及「从这一档第一格开始」的游标。
///
/// `cursor` 是**不透明**字符串（服务端签发的键集游标）；`None` 表示这一档从第 0
/// 条开始，直接用没有 cursor 的首页。客户端不得解析或构造它。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IndexBucket {
    pub initial: String,
    pub count: u64,
    pub cursor: Option<String>,
}

/// 分档用的首字母：标题第一个字符大写；空标题归到 `#`（与 UI 索引条的末档一致）。
fn bucket_initial(title: &str) -> String {
    title
        .chars()
        .next()
        .map(|first| first.to_uppercase().to_string())
        .unwrap_or_else(|| "#".into())
}

pub(crate) fn order_by_sql(spec: &[OrderTerm]) -> String {
    spec.iter()
        .map(|term| {
            let direction = if term.descending { "DESC" } else { "ASC" };
            format!("{} {direction}", term.expr)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn order_spec(query: &LibraryQuery) -> Vec<OrderTerm> {
    let (sort_expr, sort_desc, kind) = match query.order {
        BrowseOrder::Title => ("i.title_sort", query.descending, KeyKind::Text),
        BrowseOrder::AddedAt => ("i.added_at_sort", query.descending, KeyKind::Int),
        BrowseOrder::ReleaseDate => ("i.release_date_sort", query.descending, KeyKind::Text),
        BrowseOrder::Rating => ("i.rating_sort", query.descending, KeyKind::Real),
        BrowseOrder::Runtime => ("i.runtime_sort", query.descending, KeyKind::Int),
        BrowseOrder::Size => ("i.size_sort", query.descending, KeyKind::Int),
        BrowseOrder::Resolution => ("i.resolution_rank", query.descending, KeyKind::Int),
        BrowseOrder::Random => ("i.stable_shuffle_key", query.descending, KeyKind::Text),
        BrowseOrder::LastPlayed => (
            "COALESCE(u.last_played_at, 0)",
            query.descending,
            KeyKind::Int,
        ),
    };

    let mut spec = Vec::with_capacity(3);
    if query.filter.unwatched_first {
        spec.push(OrderTerm {
            expr: super::WATCH_TIER_SQL,
            descending: false,
            kind: KeyKind::Int,
        });
    }
    spec.push(OrderTerm {
        expr: sort_expr,
        descending: sort_desc,
        kind,
    });
    spec.push(OrderTerm {
        expr: "i.media_id",
        descending: sort_desc,
        kind: KeyKind::Text,
    });
    spec
}

fn seek_clause(
    spec: &[OrderTerm],
    cursor: &PageCursor,
    bindings: &mut Bindings,
) -> Result<String, StoreError> {
    if cursor.keys.len() + 1 != spec.len() {
        return Err(malformed_cursor());
    }
    let mut values = cursor.keys.clone();
    values.push(CursorKey::Text(cursor.media_id.clone()));

    // Same-direction keys: one row-value comparison lets SQLite seek the composite
    // index range directly (O01). Each key is bound once.
    if spec
        .iter()
        .all(|term| term.descending == spec[0].descending)
    {
        let operator = if spec[0].descending { "<" } else { ">" };
        let exprs = spec
            .iter()
            .map(|term| term.expr)
            .collect::<Vec<_>>()
            .join(", ");
        let placeholders = values
            .iter()
            .map(|value| bindings.push(value.to_sql()))
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(format!("(({exprs}) {operator} ({placeholders}))"));
    }

    // Mixed directions (watch tier ASC before a DESC sort) keep the lexicographic form.
    let mut groups = Vec::with_capacity(spec.len());
    for index in 0..spec.len() {
        let mut parts = Vec::with_capacity(index + 1);
        for earlier in 0..index {
            let value = bindings.push(values[earlier].to_sql());
            parts.push(format!("{} = {value}", spec[earlier].expr));
        }
        let value = bindings.push(values[index].to_sql());
        let operator = if spec[index].descending { "<" } else { ">" };
        parts.push(format!("{} {operator} {value}", spec[index].expr));
        groups.push(format!("({})", parts.join(" AND ")));
    }
    Ok(format!("({})", groups.join(" OR ")))
}

fn id_from<T: FromStr>(row: &Row<'_>, index: usize) -> rusqlite::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    row.get::<_, String>(index)?.parse::<T>().map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
    })
}

fn kind_from(row: &Row<'_>, index: usize) -> rusqlite::Result<MediaKind> {
    row.get::<_, String>(index)?
        .parse()
        .map_err(|kind: String| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid media kind: {kind}"),
                )),
            )
        })
}

pub(crate) fn item_from_row(row: &Row<'_>) -> rusqlite::Result<LibraryItem> {
    let lib_id: LibraryId = id_from(row, 0)?;
    let media_id: MediaId = id_from(row, 1)?;
    let kind: MediaKind = kind_from(row, 2)?;
    let year: Option<i64> = row.get(5)?;
    let size: Option<i64> = row.get(12)?;
    let preferred: Option<String> = row.get(13)?;
    let rating: f64 = row.get(15)?;
    let poster: Option<String> = row.get(20)?;
    let backdrop: Option<String> = row.get(21)?;
    Ok(LibraryItem {
        library_id: lib_id,
        media_id,
        kind,
        title: row.get(3)?,
        title_sort: row.get(4)?,
        year: year.map(|value| value as u16),
        file_count: row.get::<_, i64>(6)?.max(0) as usize,
        available_file_count: row.get::<_, i64>(7)?.max(0) as usize,
        missing_file_count: row.get::<_, i64>(8)?.max(0) as usize,
        unknown_file_count: row.get::<_, i64>(9)?.max(0) as usize,
        distinct_episode_count: row.get::<_, i64>(10)?.max(0) as usize,
        distinct_season_count: row.get::<_, i64>(11)?.max(0) as usize,
        size_bytes: size.map(|value| value as u64),
        preferred_file_id: preferred.and_then(|value| value.parse().ok()),
        has_still: row.get::<_, i64>(14)? != 0,
        rating: (rating >= 0.0).then_some(rating),
        language: row.get(16)?,
        identity_state: row.get(17)?,
        missing: row.get::<_, i64>(18)? != 0,
        scraped: row.get::<_, i64>(19)? != 0,
        poster_ref: poster.and_then(|value| value.parse().ok()),
        backdrop_ref: backdrop.and_then(|value| value.parse().ok()),
        resolution_rank: row.get(22)?,
        studio: None,
        studio_logo: None,
    })
}

fn read_key(row: &Row<'_>, index: usize, kind: KeyKind) -> Result<CursorKey, StoreError> {
    match kind {
        KeyKind::Int => Ok(CursorKey::Int(row.get(index)?)),
        KeyKind::Real => Ok(CursorKey::Real(row.get(index)?)),
        KeyKind::Text => Ok(CursorKey::Text(row.get(index)?)),
    }
}

struct PageRows {
    items: Vec<LibraryItem>,
    last_keys: Option<Vec<CursorKey>>,
    truncated: bool,
}

fn read_page(
    tx: &Connection,
    sql: &str,
    bindings: Vec<SqlValue>,
    kinds: &[KeyKind],
    limit: usize,
    cursor: bool,
) -> Result<(PageRows, super::stats::QueryWorkStats), StoreError> {
    let mut statement = tx.prepare(sql)?;
    let mut items = Vec::new();
    let mut last_keys = None;
    let mut truncated = false;
    let mut rows_returned = 0u64;
    {
        let mut rows = statement.query(params_from_iter(bindings))?;
        while let Some(row) = rows.next()? {
            rows_returned += 1;
            if cursor && items.len() == limit {
                truncated = true;
                break;
            }
            let item = item_from_row(row)?;
            let mut keys = Vec::with_capacity(kinds.len());
            for (index, kind) in kinds.iter().enumerate() {
                keys.push(read_key(row, ITEM_KEY_OFFSET + index, *kind)?);
            }
            items.push(item);
            last_keys = Some(keys);
        }
    }
    let fullscan_steps =
        u64::from(statement.get_status(rusqlite::StatementStatus::FullscanStep) as u32);
    let vm_steps = u64::from(statement.get_status(rusqlite::StatementStatus::VmStep) as u32);
    let sort_operations = u64::from(statement.get_status(rusqlite::StatementStatus::Sort) as u32);
    let stats = super::stats::QueryWorkStats {
        fullscan_steps,
        vm_steps,
        sort_operations,
        rows_returned,
        ..Default::default()
    };
    Ok((
        PageRows {
            items,
            last_keys,
            truncated,
        },
        stats,
    ))
}

fn page_statement(
    query: &LibraryQuery,
    base: &ItemQuerySql,
    spec: &[OrderTerm],
) -> Result<(String, usize, Vec<SqlValue>), StoreError> {
    let keys = spec[..spec.len() - 1]
        .iter()
        .map(|term| term.expr)
        .collect::<Vec<_>>()
        .join(", ");
    let columns = format!("{ITEM_COLUMNS}, {keys}");
    let mut bindings = Bindings {
        values: base.bindings.clone(),
    };
    let (sql, limit) = match &query.page {
        BrowsePage::Offset { start, limit } => {
            let limit = i64::try_from(*limit).unwrap_or(i64::MAX);
            let offset = i64::try_from(*start).unwrap_or(i64::MAX);
            let limit_ph = bindings.push(limit.into());
            let offset_ph = bindings.push(offset.into());
            (
                format!(
                    "{} ORDER BY {} LIMIT {limit_ph} OFFSET {offset_ph}",
                    base.select(&columns),
                    order_by_sql(spec)
                ),
                usize::MAX,
            )
        }
        BrowsePage::Cursor { after, limit } => {
            let limit = (*limit).clamp(1, MAX_ITEM_PAGE);
            let seek = match after.as_deref() {
                Some(raw) => {
                    let cursor = cursor::decode(raw, &cursor::scope(query)?, spec)?;
                    format!(" AND {}", seek_clause(spec, &cursor, &mut bindings)?)
                }
                None => String::new(),
            };
            let rows = i64::try_from(limit + 1).unwrap_or(i64::MAX);
            let rows_ph = bindings.push(rows.into());
            (
                format!(
                    "{} {seek} ORDER BY {} LIMIT {rows_ph}",
                    base.select(&columns),
                    order_by_sql(spec)
                ),
                limit,
            )
        }
    };
    Ok((sql, limit, bindings.values))
}

pub(crate) fn ensure_library_query_ready_on(
    conn: &Connection,
    library_id: LibraryId,
) -> Result<(), StoreError> {
    let state: Option<String> = conn
        .query_row(
            "SELECT lifecycle_state FROM libraries WHERE library_id=?1",
            [library_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if state.as_deref() == Some("reindexing") {
        return Err(StoreError::QueryUnavailable { library_id });
    }
    Ok(())
}

pub(crate) fn library_item_page_on(
    conn: &Connection,
    query: &LibraryQuery,
) -> Result<(ItemPage, super::stats::QueryWorkStats), StoreError> {
    ensure_library_query_ready_on(conn, query.library_id)?;
    let (integrity, op_id): (String, Option<String>) = conn.query_row(
        "SELECT l.integrity_state,
         (SELECT operation_id FROM library_operations WHERE library_id=l.library_id AND kind='integrity' AND state IN ('failed','succeeded') ORDER BY updated_at_ms DESC LIMIT 1)
         FROM libraries l WHERE l.library_id=?1", [query.library_id.to_string()],
        |r| Ok((r.get(0)?,r.get(1)?))).optional()?.unwrap_or_else(|| ("unknown".into(),None));
    if integrity == "failed" {
        return Err(StoreError::Protected(format!(
            "library.integrity: library {} integrity check failed ({})",
            query.library_id,
            op_id.unwrap_or_default()
        )));
    }
    let base = ItemQuerySql::build_for_order(
        query.library_id,
        &query.filter,
        query.user_id,
        matches!(query.order, BrowseOrder::LastPlayed) || query.filter.unwatched_first,
    )?;
    let total = if query.include_total {
        Some(crate::library_query::members::count_browse(
            conn,
            &crate::library_query::members::plan_browse(
                query.library_id,
                query.user_id,
                &query.filter,
            )?,
        )?)
    } else {
        None
    };
    let spec = order_spec(query);
    let (sql, limit, values) = page_statement(query, &base, &spec)?;
    let kinds: Vec<KeyKind> = spec[..spec.len() - 1]
        .iter()
        .map(|term| term.kind)
        .collect();
    let (page, stats) = read_page(
        conn,
        &sql,
        values,
        &kinds,
        limit,
        matches!(query.page, BrowsePage::Cursor { .. }),
    )?;
    let next_cursor = match (page.truncated, page.last_keys) {
        (true, Some(keys)) => {
            let media_id = page
                .items
                .last()
                .map(|item| item.media_id.to_string())
                .ok_or_else(malformed_cursor)?;
            Some(cursor::encode(&cursor::scope(query)?, keys, &media_id)?)
        }
        _ => None,
    };
    tracing::debug!(library = %query.library_id, total, items = page.items.len(), "browse page complete");
    Ok((
        ItemPage {
            items: page.items,
            total,
            next_cursor,
        },
        stats,
    ))
}

pub(crate) fn library_item_count_on(
    conn: &Connection,
    query: &LibraryQuery,
) -> Result<u64, StoreError> {
    ensure_library_query_ready_on(conn, query.library_id)?;
    let plan =
        crate::library_query::members::plan_browse(query.library_id, query.user_id, &query.filter)?;
    crate::library_query::members::count_browse(conn, &plan)
}

impl Store {
    pub fn library_item_page(&self, query: &LibraryQuery) -> Result<ItemPage, StoreError> {
        self.with_library_read(|db| db.library_item_page(query))
    }

    pub fn library_item_page_with_stats(
        &self,
        query: &LibraryQuery,
    ) -> Result<(ItemPage, super::stats::QueryWorkStats), StoreError> {
        let (page, mut stats) = self.with_stats(crate::stats::StatsScope::Both, || {
            self.library_item_page_inner(query)
        });
        let (page, page_stats) = page?;
        stats.rows_returned = page_stats.rows_returned;
        Ok((page, stats))
    }

    fn library_item_page_inner(
        &self,
        query: &LibraryQuery,
    ) -> Result<(ItemPage, super::stats::QueryWorkStats), StoreError> {
        self.with_library_read(|db| db.library_item_page_inner(query))
    }

    /// 浏览顺序下的标题快照（顺序断言、诊断用；分档请用 [`Self::library_item_index`]）。
    pub fn library_item_titles(&self, query: &LibraryQuery) -> Result<Vec<String>, StoreError> {
        self.with_library_read(|db| db.library_item_titles(query))
    }

    /// 与浏览页同一份顺序的首字母分档，并带出每档的**起始游标**。
    ///
    /// 游标语义是「严格排在游标那一格之后」（见 `seek_clause`），因此每档带的是
    /// **上一档最后一格**的游标；第一档没有前驱，`None` 就是首页。这样一次 A-Z
    /// 跳转 = 一个浏览页请求（Task 26），不再需要按 offset 逐页往前走；绝对下标
    /// 留在 store 内部，线上只有不透明游标。
    pub fn library_item_index(&self, query: &LibraryQuery) -> Result<Vec<IndexBucket>, StoreError> {
        self.with_library_read(|db| db.library_item_index(query))
    }

    pub fn library_item_count(&self, query: &LibraryQuery) -> Result<u64, StoreError> {
        self.with_library_read(|db| db.library_item_count(query))
    }
}

pub(crate) fn library_item_index_on(
    conn: &rusqlite::Connection,
    query: &LibraryQuery,
) -> Result<Vec<IndexBucket>, StoreError> {
    ensure_library_query_ready_on(conn, query.library_id)?;
    let base = ItemQuerySql::build_for_order(
        query.library_id,
        &query.filter,
        query.user_id,
        matches!(query.order, BrowseOrder::LastPlayed) || query.filter.unwatched_first,
    )?;
    let spec = order_spec(query);
    let cursor_scope = cursor::scope(query)?;
    let kinds: Vec<KeyKind> = spec.iter().map(|term| term.kind).collect();
    let keys_sql = spec
        .iter()
        .map(|term| term.expr)
        .collect::<Vec<_>>()
        .join(", ");
    // 排序键与 `i.title` 一起取：分档按标题，游标按排序键。
    let columns = format!("{keys_sql}, i.title");
    let sql = format!("{} ORDER BY {}", base.select(&columns), order_by_sql(&spec));
    let mut statement = conn.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(base.bindings.iter().cloned()))?;
    let mut buckets: Vec<IndexBucket> = Vec::new();
    // 上一行的游标：它就是下一档「从这一格开始」要用的游标。
    let mut previous_cursor: Option<String> = None;
    while let Some(row) = rows.next()? {
        let mut keys = Vec::with_capacity(kinds.len());
        for (index, kind) in kinds.iter().enumerate() {
            keys.push(read_key(row, index, *kind)?);
        }
        // 顺序定义的最后一项恒为 `i.media_id`（`order_spec` 保证），游标把它
        // 单独存一份，不再放进 `keys`。
        let Some(CursorKey::Text(media_id)) = keys.pop() else {
            return Err(malformed_cursor());
        };
        let title: String = row.get(kinds.len())?;
        let initial = bucket_initial(&title);
        let this_cursor = cursor::encode(&cursor_scope, keys, &media_id)?;
        match buckets.last_mut() {
            Some(last) if last.initial == initial => last.count += 1,
            _ => buckets.push(IndexBucket {
                initial,
                count: 1,
                cursor: previous_cursor.clone(),
            }),
        }
        previous_cursor = Some(this_cursor);
    }
    Ok(buckets)
}

#[path = "page/connection.rs"]
mod connection;
