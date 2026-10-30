//! 谓词：把协议查询的剩余合同字段翻成 `rows` 上的 SQL 子句。
//!
//! 每个字段只在这里决定"怎么筛"，取值顺序与 `matches_filter_predicates`
//! （`crates/media-server/src/provider/query.rs`）保持一致，避免两份语义。
//! 分类成员由 [`super::membership`] 组装，搜索才请求冷字段。

use rusqlite::types::Value as SqlValue;

use domain::{ProtocolItemQuery, ProtocolItemType, ProtocolMarkFilter, ProtocolParent};

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
    request: &ProtocolItemQuery,
    exact: &str,
    branch: Branch,
    bindings: &mut Bindings,
) -> Result<String, StoreError> {
    let mut clauses = Vec::new();
    kind_clauses(request, branch, &mut clauses);
    folder_clause(request, branch, bindings, &mut clauses);
    season_clause(request, branch, bindings, &mut clauses);
    clauses.push(exact.to_string());
    search_clause(request, branch, bindings, &mut clauses);
    mark_clauses(request, branch, bindings, &mut clauses);
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
    // 作品支先用写入期 `search_text`（跨语言的折叠干草堆）收窄，再跑按语言链的精确判断。
    let prefilter = if branch.unit() {
        String::new()
    } else {
        format!("instr(s.search_text, {needle}) > 0 AND ")
    };
    clauses.push(format!("({prefilter}({}))", parts.join(" OR ")));
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
///
/// 作品支上的剧（R12）按集归约，与展示侧 `unit_counts_from_snapshot` 一致：
/// 本库每个已入库分集的有效断言都是已看，剧才算已看（没有分集时同样算已看）。
fn played_clause(branch: Branch, user: &str, value: i64, bindings: &mut Bindings) -> String {
    let expected = bindings.push(SqlValue::Integer(value));
    if branch.unit() {
        return format!("{} = {expected}", effective_played_sql(user, Some("u")));
    }
    // Work summaries already reduce inherited episode assertions at write time.
    // Preserve the protocol's vacuous-played rule for a TV work with no units.
    format!(
        "((s.kind = 'tv' AND NOT EXISTS (SELECT 1 FROM library_units ue \
           WHERE ue.library_id = s.library_id AND ue.media_id = s.media_id \
             AND ue.season >= 0 AND ue.episode >= 0)) OR \
         EXISTS (SELECT 1 FROM user_library_items ui WHERE ui.user_id = {user} \
           AND ui.library_id = s.library_id AND ui.media_id = s.media_id AND ui.played = 1)) = {expected}"
    )
}

/// 一个单元（`unit` 别名给出季/集，`None` 是整个作品）的有效已看值，0/1。
///
/// 每个可继承坐标（集 / 季 / 整个作品）各按完整主键 seek 一次，最多合并三条候选再决议，
/// 不扫描同 User/Media 的全部断言：最新 `played_changed_at_ms` 胜，同一时刻 集 > 季 > 整个作品。
/// 三条候选的 season 只能是 `{unit}.season` 或 -1，episode 只能是 `{unit}.episode` 或 -1，
/// 所以按 (season, episode) 降序即是该优先级。
fn effective_played_sql(user: &str, unit: Option<&str>) -> String {
    let seek = |season: &str, episode: &str| {
        format!(
            "SELECT pu.played, pu.played_changed_at_ms AS at, pu.season AS se, pu.episode AS ep \
               FROM playback_units pu \
              WHERE pu.user_id = {user} AND pu.media_id = s.media_id \
                AND pu.season = {season} AND pu.episode = {episode} AND pu.has_played_assertion = 1"
        )
    };
    let candidates = match unit {
        Some(u) => format!(
            "{} UNION ALL {} UNION ALL {}",
            seek(&format!("{u}.season"), &format!("{u}.episode")),
            seek(&format!("{u}.season"), "-1"),
            seek("-1", "-1")
        ),
        None => seek("-1", "-1"),
    };
    format!(
        "COALESCE((SELECT c.played FROM ({candidates}) c \
           ORDER BY COALESCE(c.at, 0) DESC, c.se DESC, c.ep DESC LIMIT 1), 0)"
    )
}

/// 收藏只认本行（集行看集、媒体行看整部），与轻量投影里的 `favorite_unit` 一致。
fn favorite_clause(branch: Branch, user: &str, value: i64) -> String {
    let unit = if branch.unit() {
        "pu.season = u.season AND pu.episode = u.episode"
    } else {
        "pu.season = -1 AND pu.episode = -1"
    };
    let exact = format!(
        "EXISTS (SELECT 1 FROM playback_units pu WHERE pu.user_id = {user} \
           AND pu.media_id = s.media_id AND {unit} AND pu.favorite = 1)"
    );
    let exists = if branch.unit() {
        exact
    } else {
        // A summary can also be favorite because of one episode. The whole-work
        // assertion keeps protocol favorites exact while the summary bounds work.
        format!(
            "(EXISTS (SELECT 1 FROM user_library_items ui WHERE ui.user_id = {user} AND ui.library_id = s.library_id AND ui.media_id = s.media_id AND ui.favorite = 1) AND {exact})"
        )
    };
    if value == 1 {
        exists
    } else {
        format!("NOT {exists}")
    }
}

fn quoted_list(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| format!("'{value}'"))
        .collect::<Vec<_>>()
        .join(", ")
}
