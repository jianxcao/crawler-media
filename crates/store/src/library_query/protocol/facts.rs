//! 人物与流派谓词的取值解析。
//!
//! Jellyfin 侧的人/流派是 UUID，本地表里存的是名字，两边靠既有派生规则对齐：
//! 人物 id = `uuid5(NAMESPACE_URL, "tmdb:{id}:{type}" | "name:{lower}:{type}")`，
//! 流派 id = `uuid5(NAMESPACE_DNS, name)`（简单/带横线/`genre_` 前缀三种写法）。
//!
//! 解析不做全库扫描以外的事：先取出**当前范围内**出现过的人/流派（有界），
//! 在 Rust 里算派生 id 并只保留命中的那些，再让分页 SQL 用 `IN (...)` 命中。
//! 这样不需要 rusqlite 自定义函数，也不需要把 uuid 计算塞进 SQL。

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;
use rusqlite::types::Value as SqlValue;

use domain::ProtocolItemQuery;
use uuid::Uuid;

use super::scope::Scope;
use super::sql::scoped_cte;
use crate::StoreError;
use crate::library_query::items::Bindings;
use crate::protocol_index::{compact_id, person_wire_id, requested_genre_keys, role_from_wire};

/// 一个角色上解析出的命中集合：名字按小写比较，`tmdb` 只对演员有效。
#[derive(Default)]
struct RoleFilter {
    names: Vec<String>,
    tmdb: Vec<String>,
}

/// 人物谓词；`None` 表示这次请求没有人物过滤。
pub(super) fn people_clause(
    conn: &Connection,
    request: &ProtocolItemQuery,
    scope: &Scope,
    bindings: &mut Bindings,
) -> Result<Option<String>, StoreError> {
    if request.person_ids.is_empty() {
        return Ok(None);
    }
    let wanted: BTreeSet<String> = request.person_ids.iter().map(|id| compact_id(id)).collect();
    let keys: Vec<String> = wanted.iter().cloned().collect();
    let roles = resolve_people(conn, scope, &wanted, &keys)?;
    let mut clauses = Vec::new();
    for (role, filter) in roles {
        if let Some(clause) = role_clause(role, &filter, bindings) {
            clauses.push(clause);
        }
    }
    if clauses.is_empty() {
        // 请求里的人物一条都没落在范围内：命中集合为空，与旧实现的 any() 语义一致。
        return Ok(Some("1 = 0".to_string()));
    }
    let prefilter = index_prefilter(PERSON_KINDS, &string_list(bindings, &keys));
    // 旧实现是「任一元数据人物命中请求集合」，跨角色取并集；写入期索引先把候选收窄。
    Ok(Some(format!(
        "({prefilter} AND ({}))",
        clauses.join(" OR ")
    )))
}

/// 流派谓词；`requested` 已由调用方合并 `genres`/`genre_ids`/`Genre` 父级。
pub(super) fn genre_clause(
    conn: &Connection,
    requested: &[String],
    scope: &Scope,
    bindings: &mut Bindings,
) -> Result<Option<String>, StoreError> {
    if requested.is_empty() {
        return Ok(None);
    }
    let keys: Vec<String> = requested
        .iter()
        .flat_map(|wanted| requested_genre_keys(wanted))
        .collect();
    // A display name already is the exact predicate value. Only wire UUIDs
    // need resolving against stored metadata; avoid parsing every matching
    // Media's JSON twice for ordinary named genre requests.
    let wire_values: Vec<String> = requested
        .iter()
        .filter(|wanted| {
            let normalized = wanted.trim().to_lowercase();
            Uuid::parse_str(normalized.strip_prefix("genre_").unwrap_or(&normalized)).is_ok()
        })
        .cloned()
        .collect();
    let mut matched = requested.to_vec();
    if !wire_values.is_empty() {
        matched.extend(resolve_genres(conn, &wire_values, scope, &keys)?);
    }
    if matched.is_empty() {
        return Ok(Some("1 = 0".to_string()));
    }
    let prefilter = index_prefilter(GENRE_KINDS, &string_list(bindings, &keys));
    let list = lowercase_list(bindings, &matched);
    let sources = [
        "EXISTS (SELECT 1 FROM library_item_terms t \
           WHERE t.library_id = s.library_id AND t.media_id = s.media_id \
             AND t.term_kind = 'genre' AND casefold(t.term_value) IN ({list}))",
        "EXISTS (SELECT 1 FROM json_each(COALESCE(s.adopted_genres, '[]')) g \
           WHERE {genre_expr} IN ({list}))",
        "EXISTS (SELECT 1 FROM json_each(COALESCE(s.nfo_json, '{}'), '$.genres.value') g \
           WHERE {genre_expr} IN ({list}))",
    ];
    let mut clauses = Vec::new();
    for source in sources {
        clauses.push(
            source
                .replace("{genre_expr}", genre_expr())
                .replace("{list}", &list),
        );
    }
    Ok(Some(format!(
        "({prefilter} AND ({}))",
        clauses.join(" OR ")
    )))
}

const GENRE_KINDS: &str = "'jellyfin_genre'";
const PERSON_KINDS: &str = "'person_id', 'person_tmdb_id'";

/// 写入期协议索引（`crate::protocol_index`）上的候选集：跨所有语言收录，是精确谓词的
/// 必要条件，所以先用它收窄，再对少量候选跑按语言链取值的精确判断。
fn index_prefilter(kinds: &str, list: &str) -> String {
    format!(
        "(s.library_id, s.media_id) IN (SELECT t.library_id, t.media_id FROM library_item_terms t \
           WHERE t.term_kind IN ({kinds}) AND t.term_value IN ({list}))"
    )
}

/// 解析查询专用：`scoped` 再按索引候选收窄，占位符接在作用域参数之后。
fn candidates_cte(scope: &Scope, kinds: &str, keys: &[String]) -> (String, Vec<SqlValue>) {
    let mut values = scope.values.clone();
    let list = keys
        .iter()
        .map(|key| {
            values.push(SqlValue::Text(key.clone()));
            format!("?{}", values.len())
        })
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "WITH {}, cand AS (SELECT * FROM scoped s WHERE {})",
        scoped_cte(scope),
        index_prefilter(kinds, &list)
    );
    (sql, values)
}

/// 流派元素既可能是 `{"name": ...}` 对象，也可能是裸字符串；比较用折叠形。
fn genre_expr() -> &'static str {
    "casefold(CASE g.type WHEN 'object' THEN json_extract(g.value, '$.name') \
      WHEN 'text' THEN g.value END)"
}

/// 原串流派名：协议 id 由显示名派生，解析时不能先折叠（R23）。
fn genre_raw_expr() -> &'static str {
    "CASE g.type WHEN 'object' THEN json_extract(g.value, '$.name') \
      WHEN 'text' THEN g.value END"
}

/// 范围内出现过的人物 → 命中请求集合的 (角色, 名字/tmdb)。
fn resolve_people(
    conn: &Connection,
    scope: &Scope,
    wanted: &BTreeSet<String>,
    keys: &[String],
) -> Result<BTreeMap<&'static str, RoleFilter>, StoreError> {
    let (with, values) = candidates_cte(scope, PERSON_KINDS, keys);
    let sql = format!(
        "{with} \
         SELECT ptype, name, tmdb FROM (\
           SELECT 'Actor' AS ptype, json_extract(j.value, '$.name') AS name, \
             json_extract(j.value, '$.tmdb_id') AS tmdb \
             FROM cand s, json_each(COALESCE(s.nfo_json, '{{}}'), '$.cast.value') j \
           UNION SELECT 'Actor', json_extract(j.value, '$.name'), json_extract(j.value, '$.tmdb_id') \
             FROM cand s, json_each(COALESCE(s.adopted_json, '{{}}'), '$.fields.cast.value') j \
           UNION SELECT 'Director', j.value, NULL \
             FROM cand s, json_each(COALESCE(s.nfo_json, '{{}}'), '$.directors.value') j \
           UNION SELECT 'Director', j.value, NULL \
             FROM cand s, json_each(COALESCE(s.adopted_json, '{{}}'), '$.fields.directors.value') j \
           UNION SELECT 'Writer', j.value, NULL \
             FROM cand s, json_each(COALESCE(s.nfo_json, '{{}}'), '$.creators.value') j \
           UNION SELECT 'Writer', j.value, NULL \
             FROM cand s, json_each(COALESCE(s.adopted_json, '{{}}'), '$.fields.creators.value') j\
         )"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut resolved: BTreeMap<&'static str, RoleFilter> = BTreeMap::new();
    for row in rows {
        let (role, name, tmdb) = row?;
        let Some(role) = role_from_wire(&role) else {
            continue;
        };
        let Some(name) = name.filter(|name| !name.trim().is_empty()) else {
            continue;
        };
        let wire = person_wire_id(role, &name, tmdb.as_deref());
        let hit = wanted.contains(&compact_id(&wire))
            || tmdb
                .as_deref()
                .is_some_and(|tmdb| wanted.contains(tmdb.trim()));
        if !hit {
            continue;
        }
        let filter = resolved.entry(role).or_default();
        push_unique(&mut filter.names, name.trim().to_lowercase());
        if let Some(tmdb) = tmdb {
            push_unique(&mut filter.tmdb, tmdb.trim().to_string());
        }
    }
    Ok(resolved)
}

/// 范围内出现过的流派 → 命中请求值的名字。
fn resolve_genres(
    conn: &Connection,
    requested: &[String],
    scope: &Scope,
    keys: &[String],
) -> Result<Vec<String>, StoreError> {
    let (with, values) = candidates_cte(scope, GENRE_KINDS, keys);
    let sql = format!(
        "{with} \
         SELECT name FROM (\
           SELECT t.term_value AS name \
             FROM library_item_terms t \
             JOIN cand s ON s.library_id = t.library_id AND s.media_id = t.media_id \
             WHERE t.term_kind = 'genre' \
           UNION SELECT {expr} \
             FROM cand s, json_each(COALESCE(s.adopted_genres, '[]')) g \
           UNION SELECT {expr} \
             FROM cand s, json_each(COALESCE(s.nfo_json, '{{}}'), '$.genres.value') g\
         ) WHERE name IS NOT NULL AND trim(name) != ''",
        expr = genre_raw_expr()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values), |row| {
        row.get::<_, String>(0)
    })?;
    let mut matched = Vec::new();
    for row in rows {
        let name = row?;
        if requested.iter().any(|wanted| genre_hits(wanted, &name)) {
            push_unique(&mut matched, name);
        }
    }
    Ok(matched)
}

/// 单个请求值是否命中候选流派名（名字或由名字派生的三种 id 写法）。
fn genre_hits(wanted: &str, name: &str) -> bool {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return false;
    }
    if name.to_lowercase() == wanted.to_lowercase() {
        return true;
    }
    derived_genre_ids(name)
        .iter()
        .any(|id| id.eq_ignore_ascii_case(wanted))
}

/// `uuid5(NAMESPACE_DNS, name)` 的简单/带横线/`genre_` 前缀写法。
fn derived_genre_ids(name: &str) -> [String; 3] {
    let uuid = Uuid::new_v5(&Uuid::NAMESPACE_DNS, name.as_bytes());
    let simple = uuid.simple().to_string();
    [
        simple.clone(),
        uuid.hyphenated().to_string(),
        format!("genre_{simple}"),
    ]
}

/// 角色谓词：任一来源命中解析出的名字（演员额外比 tmdb 号）。
fn role_clause(role: &str, filter: &RoleFilter, bindings: &mut Bindings) -> Option<String> {
    let mut parts = Vec::new();
    if !filter.names.is_empty() {
        let list = lowercase_list(bindings, &filter.names);
        for (json, path, keyed) in role_sources(role) {
            let expr = if keyed {
                "casefold(json_extract(j.value, '$.name'))"
            } else {
                "casefold(j.value)"
            };
            parts.push(format!(
                "EXISTS (SELECT 1 FROM json_each({json}, '{path}') j WHERE {expr} IN ({list}))"
            ));
        }
    }
    if !filter.tmdb.is_empty() {
        let list = string_list(bindings, &filter.tmdb);
        parts.push(format!(
            "EXISTS (SELECT 1 FROM json_each(COALESCE(s.nfo_json, '{{}}'), '$.cast.value') j \
             WHERE json_extract(j.value, '$.tmdb_id') IN ({list}))"
        ));
        parts.push(format!(
            "EXISTS (SELECT 1 FROM json_each(COALESCE(s.adopted_json, '{{}}'), '$.fields.cast.value') j \
             WHERE json_extract(j.value, '$.tmdb_id') IN ({list}))"
        ));
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("({})", parts.join(" OR ")))
}

/// 角色 → (JSON 文本表达式, 元素路径, 元素是否带 `name` 键)。
fn role_sources(role: &str) -> Vec<(&'static str, &'static str, bool)> {
    match role {
        "Actor" => vec![
            ("COALESCE(s.nfo_json, '{}')", "$.cast.value", true),
            (
                "COALESCE(s.adopted_json, '{}')",
                "$.fields.cast.value",
                true,
            ),
        ],
        "Director" => vec![
            ("COALESCE(s.nfo_json, '{}')", "$.directors.value", false),
            (
                "COALESCE(s.adopted_json, '{}')",
                "$.fields.directors.value",
                false,
            ),
        ],
        _ => vec![
            ("COALESCE(s.nfo_json, '{}')", "$.creators.value", false),
            (
                "COALESCE(s.adopted_json, '{}')",
                "$.fields.creators.value",
                false,
            ),
        ],
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

/// 绑定一组小写化字符串，返回 `?n, ?m` 占位符。
fn lowercase_list(bindings: &mut Bindings, values: &[String]) -> String {
    let lowered: Vec<String> = values.iter().map(|value| value.to_lowercase()).collect();
    string_list(bindings, &lowered)
}

fn string_list(bindings: &mut Bindings, values: &[String]) -> String {
    values
        .iter()
        .map(|value| bindings.push(SqlValue::Text(value.clone())))
        .collect::<Vec<_>>()
        .join(", ")
}
