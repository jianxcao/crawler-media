//! 谓词：把协议查询的剩余合同字段翻成 `rows` 上的 SQL 子句。
//!
//! 每个字段只在这里决定"怎么筛"，取值顺序与 `matches_filter_predicates`
//! （`crates/media-server/src/provider/query.rs`）保持一致，避免两份语义。
//! 复杂取值（人物、流派）交给 [`super::facts`] 解析。

use rusqlite::Connection;
use rusqlite::types::Value as SqlValue;

use domain::{ProtocolItemQuery, ProtocolItemType, ProtocolMarkFilter, ProtocolParent};

use super::facts;
use super::scope::Scope;
use crate::StoreError;
use crate::library_query::items::Bindings;

/// 谓词作用在哪一支 `rows` 上：分集支有 `library_units` 的 `u` 别名与季/集坐标，
/// 作品支没有；同一请求展开两支时，同一字段要按各自语义分别翻译。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Branch {
    Work,
    Unit,
}

impl Branch {
    fn unit(self) -> bool {
        self == Self::Unit
    }
}

/// 组装某一支 `rows` 的完整谓词；没有任何条件时给 `1 = 1`。
pub(super) fn build(
    conn: &Connection,
    request: &ProtocolItemQuery,
    scope: &Scope,
    branch: Branch,
    bindings: &mut Bindings,
) -> Result<String, StoreError> {
    let mut clauses = Vec::new();
    kind_clauses(request, branch, &mut clauses);
    folder_clause(request, branch, bindings, &mut clauses);
    season_clause(request, branch, bindings, &mut clauses);
    studio_clause(request, bindings, &mut clauses);
    if let Some(clause) = facts::genre_clause(conn, &genre_values(request), scope, bindings)? {
        clauses.push(clause);
    }
    search_clause(request, branch, bindings, &mut clauses);
    mark_clauses(request, branch, bindings, &mut clauses);
    if let Some(clause) = facts::people_clause(conn, request, scope, bindings)? {
        clauses.push(clause);
    }
    Ok(if clauses.is_empty() {
        "1 = 1".to_string()
    } else {
        clauses.join(" AND ")
    })
}

/// `IncludeItemTypes` 与 `IsFolder`：本地没有目录实体，两者都只看媒体种类。
///
/// 两支的类型表不同：作品支认 `Movie`/`Series`/`Video`，分集支只认 `Episode`。
/// `IsFolder` 按 `is_series` 判定，而分集行的 `is_series` 是 false，所以
/// `IsFolder=false`（"只要非剧集"）在分集支上**不设条件**，否则会清空整页。
fn kind_clauses(request: &ProtocolItemQuery, branch: Branch, clauses: &mut Vec<String>) {
    if branch.unit() {
        clauses.push("s.kind = 'tv'".to_string());
    }
    let table = [
        (ProtocolItemType::Movie, "movie", false),
        (ProtocolItemType::Series, "tv", false),
        (ProtocolItemType::Season, "tv", false),
        (ProtocolItemType::Episode, "tv", true),
        (ProtocolItemType::Video, "video", false),
    ];
    let mut kinds: Vec<&str> = table
        .iter()
        .filter(|(kind, _, unit_only)| {
            request.include_types.contains(kind) && *unit_only == branch.unit()
        })
        .map(|(_, name, _)| *name)
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    if !kinds.is_empty() {
        clauses.push(format!("s.kind IN ({})", quoted_list(&kinds)));
    }
    match (request.is_folder, branch) {
        (Some(true), Branch::Unit) => clauses.push("1 = 0".to_string()),
        (Some(true), Branch::Work) => clauses.push("s.kind = 'tv'".to_string()),
        (Some(false), Branch::Work) => clauses.push("s.kind <> 'tv'".to_string()),
        (Some(false), Branch::Unit) | (None, _) => {}
    }
}

/// `Folder` 父级：该媒体在指定根下有文件（分集请求按分集归属判断）。
fn folder_clause(
    request: &ProtocolItemQuery,
    branch: Branch,
    bindings: &mut Bindings,
    clauses: &mut Vec<String>,
) {
    let ProtocolParent::Folder { root_id } = &request.parent else {
        return;
    };
    let root = bindings.push(SqlValue::Text(root_id.clone()));
    if branch.unit() {
        clauses.push(format!(
            "EXISTS (SELECT 1 FROM library_file_units fu \
               JOIN library_files ff ON ff.file_id = fu.file_id \
               WHERE fu.library_id = s.library_id AND fu.media_id = s.media_id \
                 AND fu.season = u.season AND fu.episode = u.episode AND ff.root_id = {root})"
        ));
    } else {
        clauses.push(format!(
            "EXISTS (SELECT 1 FROM library_files ff \
               WHERE ff.library_id = s.library_id AND ff.media_id = s.media_id \
                 AND ff.root_id = {root})"
        ));
    }
}

/// `SeasonId` 参数：钉住季号（媒体已在作用域里钉住，季号只有分集支有）。
fn season_clause(
    request: &ProtocolItemQuery,
    branch: Branch,
    bindings: &mut Bindings,
    clauses: &mut Vec<String>,
) {
    let (Some((_, season)), true) = (request.season, branch.unit()) else {
        return;
    };
    let season = bindings.push(SqlValue::Integer(i64::from(season)));
    clauses.push(format!("u.season = {season}"));
}

/// `StudioIds` 与 `Studio` 父级：术语表或 NFO/采纳元数据的 `studios` 命中即算。
fn studio_clause(request: &ProtocolItemQuery, bindings: &mut Bindings, clauses: &mut Vec<String>) {
    let mut values: Vec<String> = request.studio_ids.clone();
    if let ProtocolParent::Studio(name) = &request.parent {
        values.push(name.clone());
    }
    let values = normalized(&values);
    if values.is_empty() {
        return;
    }
    let list = lowercase_list(bindings, &values);
    let mut parts = vec![format!(
        "EXISTS (SELECT 1 FROM library_item_terms t \
           WHERE t.library_id = s.library_id AND t.media_id = s.media_id \
             AND t.term_kind = 'studio' AND casefold(t.term_value) IN ({list}))"
    )];
    // NFO 输入存裸 `MetadataFields`（`{"state":"value","value":[…]}`），采纳元数据存
    // `{"fields":…}`；`record_scraped_display` 写的是裸数组。三种形状都要认，否则
    // 采纳来的 studio 会被静默丢掉。
    for (json, path) in [
        ("COALESCE(s.nfo_json, '{}')", "$.studios.value"),
        ("COALESCE(s.adopted_json, '{}')", "$.fields.studios.value"),
        ("COALESCE(s.adopted_json, '{}')", "$.fields.studios"),
    ] {
        parts.push(format!(
            "EXISTS (SELECT 1 FROM json_each({json}, '{path}') g \
             WHERE casefold(g.value) IN ({list}))"
        ));
    }
    clauses.push(format!("({})", parts.join(" OR ")));
}

/// `Genres`/`GenreIds`/`Genre` 父级合并后的请求值。
fn genre_values(request: &ProtocolItemQuery) -> Vec<String> {
    let mut values = request.genres.clone();
    values.extend(request.genre_ids.iter().cloned());
    if let ProtocolParent::Genre(name) = &request.parent {
        values.push(name.clone());
    }
    normalized(&values)
}

/// `SearchTerm`：标题、原始标题、简介任一包含即算（Unicode casefold）。
///
/// 作品支看作品级字段；分集支看**本集**的标题与简介（R23）：采纳的
/// `media_episodes` 与分集 NFO 输入，均按 `pref_locale` 语言链取一份，
/// 不再拿剧集的作品级字段或字典序首个语言冒充分集。
fn search_clause(
    request: &ProtocolItemQuery,
    branch: Branch,
    bindings: &mut Bindings,
    clauses: &mut Vec<String>,
) {
    let Some(term) = request
        .search_term
        .as_deref()
        .map(str::trim)
        .filter(|term| !term.is_empty())
    else {
        return;
    };
    let needle = bindings.push(SqlValue::Text(term.to_lowercase()));
    let fields: Vec<String> = if branch.unit() {
        // 标量子查询的 ORDER BY 不能引用外层列：偏好语言一段、字典序兜底一段。
        let episode = |column: &str| {
            let base = format!(
                "SELECT me.{column} FROM media_episodes me \
                 WHERE me.media_id = s.media_id AND me.season = u.season \
                   AND me.episode = u.episode"
            );
            format!(
                "COALESCE(({base} AND me.locale = s.pref_locale), \
                   ({base} ORDER BY me.locale LIMIT 1), '')"
            )
        };
        let nfo_base = "SELECT mi.fields_json FROM library_metadata_inputs mi \
             WHERE mi.library_id = s.library_id AND mi.media_id = s.media_id \
               AND mi.entity_scope = 'episode' AND mi.season = u.season \
               AND mi.episode = u.episode AND mi.parse_state = 'valid'";
        let nfo = format!(
            "COALESCE(({nfo_base} AND mi.locale = s.pref_locale ORDER BY mi.input_key LIMIT 1), \
               ({nfo_base} ORDER BY mi.locale, mi.input_key LIMIT 1))"
        );
        vec![
            episode("title"),
            episode("overview"),
            format!("COALESCE(json_extract({nfo}, '$.title.value'), '')"),
            format!("COALESCE(json_extract({nfo}, '$.overview.value'), '')"),
        ]
    } else {
        vec![
            "s.title".to_string(),
            "COALESCE(json_extract(s.nfo_json, '$.original_title.value'), '')".to_string(),
            "COALESCE(json_extract(s.nfo_json, '$.overview.value'), '')".to_string(),
            "COALESCE(s.adopted_original, '')".to_string(),
            "COALESCE(s.adopted_overview, '')".to_string(),
        ]
    };
    let parts: Vec<String> = fields
        .iter()
        .map(|field| format!("instr(casefold({field}), {needle}) > 0"))
        .collect();
    clauses.push(format!("({})", parts.join(" OR ")));
}

/// `Filters`/`IsFavorite`/`IsPlayed`：用户标记直接查 `playback_units`。
fn mark_clauses(
    request: &ProtocolItemQuery,
    branch: Branch,
    bindings: &mut Bindings,
    clauses: &mut Vec<String>,
) {
    let mut wants: Vec<(&str, i64)> = Vec::new();
    for filter in &request.filters {
        match filter {
            ProtocolMarkFilter::Favorite => wants.push(("favorite", 1)),
            ProtocolMarkFilter::Played => wants.push(("played", 1)),
            ProtocolMarkFilter::Unplayed => wants.push(("played", 0)),
        }
    }
    if let Some(expected) = request.is_favorite {
        wants.push(("favorite", i64::from(expected)));
    }
    if let Some(expected) = request.is_played {
        wants.push(("played", i64::from(expected)));
    }
    if wants.is_empty() {
        return;
    }
    let user = bindings.push(SqlValue::Text(request.user_id.to_string()));
    for (kind, value) in wants {
        clauses.push(if kind == "played" {
            played_clause(branch, &user, value, bindings)
        } else {
            favorite_clause(branch, &user, value)
        });
    }
}

/// `played` 读的是**有效断言**（与 [`Store::effective_played_assertion`] 同一优先级）：
/// 最新 `played_changed_at_ms` 胜，同一时刻再按 集 > 季 > 整个作品；没有任何断言行就是未看。所以未看不能用 `favorite` 那种"存在即真"。
fn played_clause(branch: Branch, user: &str, value: i64, bindings: &mut Bindings) -> String {
    let expected = bindings.push(SqlValue::Integer(value));
    format!(
        "COALESCE((SELECT pu.played FROM playback_units pu \
           WHERE pu.user_id = {user} AND pu.media_id = s.media_id AND pu.has_played_assertion = 1 \
             AND ({}) \
           ORDER BY COALESCE(pu.played_changed_at_ms, 0) DESC, \
             CASE WHEN pu.episode >= 0 THEN 2 WHEN pu.season >= 0 THEN 1 ELSE 0 END DESC \
           LIMIT 1), 0) = {expected}",
        assertion_scopes(branch)
    )
}

/// 收藏只认本行（集行看集、媒体行看整部），与轻量投影里的 `favorite_unit` 一致。
fn favorite_clause(branch: Branch, user: &str, value: i64) -> String {
    let unit = if branch.unit() {
        "pu.season = u.season AND pu.episode = u.episode"
    } else {
        "pu.season = -1 AND pu.episode = -1"
    };
    let exists = format!(
        "EXISTS (SELECT 1 FROM playback_units pu WHERE pu.user_id = {user} \
           AND pu.media_id = s.media_id AND {unit} AND pu.favorite = 1)"
    );
    if value == 1 {
        exists
    } else {
        format!("NOT {exists}")
    }
}

/// 一个单元能继承到的断言坐标，从最具体到整个作品。
fn assertion_scopes(branch: Branch) -> String {
    if branch.unit() {
        "(pu.season = u.season AND pu.episode = u.episode) \
         OR (pu.season = u.season AND pu.episode = -1) \
         OR (pu.season = -1 AND pu.episode = -1)"
            .to_string()
    } else {
        "pu.season = -1 AND pu.episode = -1".to_string()
    }
}

/// 去掉空串与重复值，保留原始大小写（比较时再折叠）。
fn normalized(values: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() || result.iter().any(|seen| seen.eq_ignore_ascii_case(value)) {
            continue;
        }
        result.push(value.to_string());
    }
    result
}

fn quoted_list(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| format!("'{value}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn lowercase_list(bindings: &mut Bindings, values: &[String]) -> String {
    values
        .iter()
        .map(|value| bindings.push(SqlValue::Text(value.to_lowercase())))
        .collect::<Vec<_>>()
        .join(", ")
}
