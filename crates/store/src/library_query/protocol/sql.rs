//! 组装协议分页的两条语句：同一条事务里算 `total`、按页取行。
//!
//! 三段 CTE 的分工是固定的，谓词只作用在 `rows` 上，去重与排序留在 SQLite 里：
//!
//! - `scoped`：媒体级事实（含上次有效 NFO 与采纳元数据），作用域在这里收敛；
//! - `rows`：按请求决定"每个媒体一行"还是"每个分集单元一行"，谓词在这里生效；
//! - `ranked`：跨库同逻辑 ID 去重，并选出可播放来源。
//!
//! `total` 不套窗口函数：分区数等于去重后的行数，`GROUP BY` 更便宜且不改变语义。

use rusqlite::types::Value as SqlValue;

use domain::{MediaId, ProtocolItemQuery, ProtocolItemRef, TypedItemId, UnitKey};

use super::order;
use super::scope::{Granularity, Scope};
use crate::library_query::items::Bindings;

/// `library_items` 加上 `pref_locale`：Library 展示语言，缺省回落实例默认语言
/// （与详情页 `library_display_locale` 同链）。别名仍是 `i`，作用域片段原样可用。
fn items_with_locale_sql(scope: &Scope) -> String {
    format!(
        "(SELECT li.*, COALESCE(NULLIF(TRIM(l.display_locale), ''), {}) AS pref_locale \
           FROM library_items li LEFT JOIN libraries l ON l.library_id = li.library_id) i",
        scope.instance_locale
    )
}

/// 上次有效的作品级 NFO 字段（`fields_json` 是裸 [`domain::MetadataFields`]）。
///
/// 同一媒体可能有多个 locale/input_key：先取 Library→实例 语言链命中的那份，
/// 其余按字典序，保证离线可重放（R23：不再按字典序取首个语言）。
///
/// SQLite 不接受标量子查询 `ORDER BY` 里引用外层列，所以偏好语言用
/// `COALESCE(命中偏好的一份, 字典序首份)` 两段表达。
const NFO_JSON: &str = "COALESCE(\
     (SELECT mi.fields_json FROM library_metadata_inputs mi \
       WHERE mi.library_id = i.library_id AND mi.media_id = i.media_id \
         AND mi.entity_scope = 'work' AND mi.parse_state = 'valid' \
         AND mi.locale = i.pref_locale \
       ORDER BY mi.input_key LIMIT 1), \
     (SELECT mi.fields_json FROM library_metadata_inputs mi \
       WHERE mi.library_id = i.library_id AND mi.media_id = i.media_id \
         AND mi.entity_scope = 'work' AND mi.parse_state = 'valid' \
       ORDER BY mi.locale, mi.input_key LIMIT 1))";

/// `scoped` 预置：媒体级事实，不读现场 NFO、不查 cache、不联网。
///
/// `pref_locale` 留给分集支谓词：分集标题/简介按同一条语言链取。
pub(super) fn scoped_cte(scope: &Scope) -> String {
    format!(
        "scoped AS (SELECT i.library_id AS library_id, i.media_id AS media_id, i.kind AS kind, \
           i.title AS title, i.added_at_sort AS added_at_sort, \
           {NFO_JSON} AS nfo_json, \
           mm.details_json AS adopted_json, mm.genres_json AS adopted_genres, \
           mm.overview AS adopted_overview, mm.original_title AS adopted_original, \
           i.pref_locale AS pref_locale \
         FROM {items} \
         LEFT JOIN media_metadata mm ON mm.media_id = i.media_id \
           AND mm.locale = COALESCE(\
             (SELECT m2.locale FROM media_metadata m2 \
               WHERE m2.media_id = i.media_id AND m2.locale = i.pref_locale), \
             (SELECT m2.locale FROM media_metadata m2 \
               WHERE m2.media_id = i.media_id ORDER BY m2.locale LIMIT 1)) \
         WHERE {})",
        scope.where_sql,
        items = items_with_locale_sql(scope),
    )
}

/// `rows` 的作品级支：每媒体一行，季/集键取 `UnitKey::WHOLE`（`-1`）。
fn work_rows_cte(predicates: &str) -> String {
    format!(
        "work_rows AS (SELECT s.library_id, s.media_id, s.kind, s.title, s.added_at_sort, \
           -1 AS season, -1 AS episode \
         FROM scoped s WHERE {predicates})"
    )
}

/// `rows` 的分集级支：每个已入库单元一行；退化单元（缺季/集号）不参与分集查询。
fn unit_rows_cte(predicates: &str) -> String {
    format!(
        "unit_rows AS (SELECT s.library_id, s.media_id, s.kind, s.title, s.added_at_sort, \
           u.season AS season, u.episode AS episode \
         FROM scoped s \
         JOIN library_units u ON u.library_id = s.library_id AND u.media_id = s.media_id \
           AND u.season >= 0 AND u.episode >= 0 \
         WHERE {predicates})"
    )
}

/// `rows`：按粒度合成一支或两支。两支列名与列序完全相同，`UNION ALL` 不改变外层语义。
///
/// 谓词由 [`plan`] 的调用方按支组装（分集支才有 `u` 别名）；这里的 `unwrap_or("1 = 0")`
/// 只保证 SQL 始终可执行——真的缺支时结果只会更小，不会越权多读。
fn rows_ctes(
    scope: &Scope,
    work_predicates: Option<&str>,
    unit_predicates: Option<&str>,
) -> String {
    match scope.granularity {
        Granularity::Work => format!(
            "{}, rows AS (SELECT * FROM work_rows)",
            work_rows_cte(work_predicates.unwrap_or("1 = 0"))
        ),
        Granularity::Unit => format!(
            "{}, rows AS (SELECT * FROM unit_rows)",
            unit_rows_cte(unit_predicates.unwrap_or("1 = 0"))
        ),
        Granularity::Mixed => format!(
            "{}, {}, rows AS (SELECT * FROM work_rows UNION ALL SELECT * FROM unit_rows)",
            work_rows_cte(work_predicates.unwrap_or("1 = 0")),
            unit_rows_cte(unit_predicates.unwrap_or("1 = 0"))
        ),
    }
}

/// 已发布且在位的可播放来源：修订号最新者胜，平局取较小的 `file_id`。
///
/// 规则与写入侧 `reduce.rs::preferred_file` / `best_art_file` 相同，但这里现算而不是读
/// `library_items.preferred_file_id`：那一列是派生摘要，摘要落后于 `library_files`
/// 时（例如修订号被原位改写）会让播放偏到旧版本。
///
/// 分集支按 `(season, episode)` 归属取文件；作品支只按媒体取，
/// 因此两支并存时必须限定 `season = -1`，免得给某个具体分集配上别的集的文件。
///
/// 状态列前的一元 `+` 让规划器放弃 `files_presence(library_id, presence_state)`：
/// 那条索引每行要扫整库文件（50k 库每页 ~13s），这里必须走 `files_library_media`。
fn source_expr(scope: &Scope) -> String {
    let mut parts = Vec::new();
    if scope.granularity.wants_unit() {
        parts.push(
            "(SELECT f.file_id FROM library_files f \
               JOIN library_file_units fu ON fu.file_id = f.file_id \
                 AND fu.season = rows.season AND fu.episode = rows.episode \
               WHERE f.library_id = rows.library_id AND f.media_id = rows.media_id \
                 AND +f.publication_state = 'published' AND +f.presence_state = 'present' \
               ORDER BY f.file_revision DESC, f.file_id ASC LIMIT 1)"
                .to_string(),
        );
    }
    if scope.granularity.wants_work() {
        let unit_guard = if scope.granularity.wants_unit() {
            "rows.season = -1 AND "
        } else {
            ""
        };
        parts.push(format!(
            "(SELECT f.file_id FROM library_files f \
               WHERE f.library_id = rows.library_id AND f.media_id = rows.media_id \
                 AND {unit_guard}+f.publication_state = 'published' \
                 AND +f.presence_state = 'present' \
               ORDER BY f.file_revision DESC, f.file_id ASC LIMIT 1)"
        ));
    }
    if parts.len() == 1 {
        return parts.pop().unwrap_or_else(|| "NULL".to_string());
    }
    format!("COALESCE({})", parts.join(", "))
}

/// 一次请求展开出的两条语句与它们的绑定参数。
pub(super) struct PagePlan {
    pub(super) count_sql: String,
    pub(super) page_sql: String,
    /// count 与 page 共用的过滤参数（顺序即 `?1..?n`）。
    pub(super) filter_values: Vec<SqlValue>,
    /// page 额外追加 `LIMIT`/`OFFSET`。
    pub(super) page_values: Vec<SqlValue>,
}

/// `limit == 0` 由 SQLite 天然解释成"不返回行"，与旧实现的 `limit > 0` 守卫一致。
pub(super) fn plan(
    request: &ProtocolItemQuery,
    scope: &Scope,
    work_predicates: Option<&str>,
    unit_predicates: Option<&str>,
    bindings: &Bindings,
) -> PagePlan {
    let scoped = scoped_cte(scope);
    let rows = rows_ctes(scope, work_predicates, unit_predicates);
    // 作品支的桶是"每媒体一次"，分集支的桶是"每媒体每单元一次"；两支桶互不相交（季号 `-1`）。
    let count_sql = format!(
        "WITH {scoped}, {rows} \
         SELECT (SELECT COUNT(*) FROM (SELECT 1 FROM rows WHERE season = -1 GROUP BY media_id)) \
              + (SELECT COUNT(*) FROM (SELECT 1 FROM rows WHERE season >= 0 \
                   GROUP BY media_id, season, episode))"
    );
    let filter_values: Vec<SqlValue> = bindings.values().collect();
    let limit_placeholder = format!("?{}", filter_values.len() + 1);
    let offset_placeholder = format!("?{}", filter_values.len() + 2);
    let order_by = order::order_by_sql(request, scope.granularity.wants_unit(), "r");
    // 授权去重与排序全部在 LIMIT 前完成；选源子查询只对本页行求值（late materialization），
    // 外层按同一全序键重排，页内顺序与 LIMIT 选择一致。
    let page_sql = format!(
        "WITH {scoped}, {rows}, ranked AS (\
           SELECT rows.*, ROW_NUMBER() OVER (\
             PARTITION BY rows.media_id, rows.season, rows.episode \
             ORDER BY (rows.kind = 'movie') DESC, rows.library_id ASC) AS rn \
           FROM rows), \
         page AS (SELECT r.* FROM ranked r WHERE r.rn = 1 ORDER BY {order_by} \
           LIMIT {limit_placeholder} OFFSET {offset_placeholder}), \
         sourced AS MATERIALIZED (SELECT rows.*, {} AS source_file_id FROM page AS rows) \
         SELECT r.library_id, r.media_id, r.season, r.episode, r.source_file_id, \
           f.file_revision AS source_revision \
         FROM sourced r LEFT JOIN library_files f ON f.file_id = r.source_file_id \
         ORDER BY {order_by}",
        source_expr(scope)
    );
    let limit = i64::try_from(request.limit).unwrap_or(i64::MAX);
    let offset = i64::try_from(request.offset).unwrap_or(i64::MAX);
    let mut page_values = filter_values.clone();
    page_values.push(SqlValue::Integer(limit));
    page_values.push(SqlValue::Integer(offset));
    PagePlan {
        count_sql,
        page_sql,
        filter_values,
        page_values,
    }
}

/// 页内一行 → 协议条目引用；`preferred_source` 缺席时保留 `None`（不可播放）。
///
/// 粒度直接读行里的季号：`season >= 0` 是分集单元，`-1` 是整个作品。混合页两支持
/// 同一条语句，所以判定不能靠外部的"这次请求是不是分集请求"。
pub(super) fn row_to_ref(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProtocolItemRef> {
    let library_id = parse_id(row.get::<_, String>(0)?, 0)?;
    let media: MediaId = parse_id(row.get::<_, String>(1)?, 1)?;
    let season: i32 = row.get(2)?;
    let episode: i32 = row.get(3)?;
    let file_id: Option<String> = row.get(4)?;
    let revision: Option<i64> = row.get(5)?;
    let preferred_source = match (file_id, revision) {
        (Some(file_id), Some(revision)) => Some(domain::PhysicalSourceId {
            file_id: parse_id(file_id, 4)?,
            revision,
        }),
        _ => None,
    };
    let item = if season >= 0 {
        TypedItemId::Unit {
            media,
            key: UnitKey::new(season, episode),
        }
    } else {
        TypedItemId::Work(media)
    };
    Ok(ProtocolItemRef {
        library_id,
        item,
        preferred_source,
    })
}

/// 行里的 UUID 文本按类型解析；损坏行变成 `rusqlite` 转换错误而不是静默跳过。
fn parse_id<T>(raw: String, index: usize) -> rusqlite::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    raw.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}
