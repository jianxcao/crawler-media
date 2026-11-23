//! 投递路由预演（POST /subscriptions/download-routing-preview）：
//! 订阅弹窗选库时预演「会下到哪、能否自动入库」。复用目录/命名模板/
//! 默认下载器配置，与真实投递同源。

use std::path::Path;
use std::str::FromStr;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{Media, MediaId, MediaKind, Release, UserId};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;
use crate::store::{DownloaderRow, Library};

#[derive(Deserialize)]
pub(crate) struct RoutingPreviewInput {
    kind: String,
    #[serde(default)]
    library_id: Option<String>,
    #[serde(default)]
    tmdb_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    year: Option<u16>,
    #[serde(default)]
    downloader_id: Option<String>,
}

struct RoutingSnapshot {
    ownership: store::LibraryOwnership,
    targetable_libraries: Vec<Library>,
    current_user: Option<UserId>,
    is_admin: bool,
    requested_downloader: Option<Option<DownloaderRow>>,
    default_downloader: Option<DownloaderRow>,
    naming_pattern: String,
    watch: Option<String>,
    inplace: Option<String>,
}

enum RuleEval {
    Skip,
    Match(String),
    Fail,
}

/// 订阅与投递目标媒体库准入：管理员（或无用户上下文调用）可定向到任何可接收工作的库；
/// 普通成员仅可定向到 `access_mode == "everyone"` 或 `member_ids` 包含自身的库。
pub(crate) fn user_can_target_library(
    library: &Library,
    user_id: Option<UserId>,
    is_admin: bool,
) -> bool {
    let Some(uid) = user_id else {
        return true;
    };
    is_admin || library.access_mode == "everyone" || library.member_ids.contains(&uid)
}

/// 自动选库与省略 `library_id` 默认落库准入：管理员（或无用户上下文）要求 `admin_visible` 为真，
/// 避免未显式选库时静默落入隐藏库；显式 `library_id` 仍走 `user_can_target_library`。
pub(crate) fn user_can_auto_route_library(
    library: &Library,
    user_id: Option<UserId>,
    is_admin: bool,
) -> bool {
    if is_admin || user_id.is_none() {
        return library.admin_visible;
    }
    match user_id {
        Some(uid) => library.access_mode == "everyone" || library.member_ids.contains(&uid),
        None => library.admin_visible,
    }
}

pub(crate) async fn preview(
    State(state): State<ApiState>,
    user_id: Option<axum::Extension<Option<UserId>>>,
    Json(body): Json<RoutingPreviewInput>,
) -> Response {
    let kind = match parse_preview_kind(&body.kind) {
        Ok(kind) => kind,
        Err(response) => return response,
    };
    let current_user = user_id.and_then(|ext| ext.0);
    let downloader = body.downloader_id.clone();
    let snapshot = match super::store_wait::store_read(
        &state,
        &[store::Db::Library, store::Db::App],
        move |store| load_snapshot(store, kind, downloader.as_deref(), current_user),
    )
    .await
    {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };
    let worker = state.clone();
    match state
        .http_work
        .run(super::library::BROWSE_DEADLINE, move || {
            evaluate_preview(&worker, kind, &body, &snapshot)
        })
        .await
    {
        Ok(response) => response,
        Err(error) => super::library::browse_work_error(error),
    }
}

fn evaluate_preview(
    state: &ApiState,
    kind: MediaKind,
    body: &RoutingPreviewInput,
    snapshot: &RoutingSnapshot,
) -> Response {
    if let Some(id) = body.library_id.as_deref().filter(|id| !id.is_empty()) {
        let Some(lib) = snapshot
            .targetable_libraries
            .iter()
            .find(|lib| lib.id == id)
        else {
            return incomplete_preview(None, "指定的媒体库不存在");
        };
        if lib.kind != kind {
            return incomplete_preview(Some(lib), "订阅媒体类型与目标库类型不匹配");
        }
        if !snapshot.ownership.accepts_work(&lib.id) {
            return incomplete_preview(Some(lib), "目标媒体库正在删除或重建");
        }
        return render_preview(kind, body, snapshot, Some(lib), false, None);
    }
    let has_rule_candidates = snapshot.targetable_libraries.iter().any(|lib| {
        lib.kind == kind
            && !lib.match_rules.is_empty()
            && snapshot.ownership.accepts_work(&lib.id)
            && user_can_auto_route_library(lib, snapshot.current_user, snapshot.is_admin)
    });
    let meta = if has_rule_candidates {
        match_metadata(state, kind, body.tmdb_id.as_deref())
    } else {
        None
    };
    let (library, matched, reason) = select_auto_library(
        &snapshot.targetable_libraries,
        &snapshot.ownership,
        kind,
        meta.as_ref(),
        snapshot.current_user,
        snapshot.is_admin,
    );
    render_preview(kind, body, snapshot, library, matched, reason)
}

fn parse_preview_kind(kind: &str) -> Result<MediaKind, Response> {
    match MediaKind::from_str(kind) {
        Ok(kind @ (MediaKind::Movie | MediaKind::Tv)) => Ok(kind),
        _ => Err(err(
            StatusCode::BAD_REQUEST,
            "routing.kind",
            "kind 必须是 movie 或 tv",
        )),
    }
}

/// 一次锁快照后续渲染所需的库归属/ACL/下载器/命名/监听配置，再释放。
/// metadata IO 必须在锁外，否则 scrape 配置会再次 `(&store)` 自锁。
fn load_snapshot(
    store: &crate::Store,
    kind: MediaKind,
    downloader_id: Option<&str>,
    current_user: Option<UserId>,
) -> Result<RoutingSnapshot, Response> {
    let ownership = store.library_ownership().map_err(|error| {
        tracing::error!(%error, "加载投递预演媒体库归属快照失败");
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        )
    })?;
    let is_admin = match current_user {
        None => true,
        Some(uid) => store.user_role(uid).map(|r| r == "admin").unwrap_or(false),
    };
    let targetable_libraries = ownership
        .libraries()
        .iter()
        .filter(|lib| user_can_target_library(lib, current_user, is_admin))
        .cloned()
        .collect();
    let requested_downloader = downloader_id.map(|raw| {
        domain::DownloaderId::from_str(raw)
            .ok()
            .and_then(|id| store.get_downloader(id).ok().flatten())
    });
    Ok(RoutingSnapshot {
        ownership,
        targetable_libraries,
        current_user,
        is_admin,
        requested_downloader,
        default_downloader: store.default_downloader().ok().flatten(),
        naming_pattern: store
            .naming_pattern(kind)
            .unwrap_or_else(|_| library::default_pattern(kind).to_string()),
        watch: store.watch_intake().ok().flatten(),
        inplace: store.watch_inplace().ok().flatten(),
    })
}

fn match_metadata(
    state: &ApiState,
    kind: MediaKind,
    tmdb_id: Option<&str>,
) -> Option<media::ItemMeta> {
    let tmdb_id = tmdb_id.filter(|id| !id.is_empty())?;
    match crate::scrape_metadata::fetch_tmdb_metadata(state, kind, tmdb_id) {
        Ok(meta) => meta,
        Err(error) => {
            tracing::warn!(%error, tmdb_id, ?kind, "投递预演拉取 TMDB 元数据失败");
            None
        }
    }
}

/// 当未显式传入 `library_id` 时，按各媒体库 `match_rules` 自动匹配目标库。
/// 先快照可接收新任务且调用者有权访问的候选库并立即释放读租约，再在锁外拉取 TMDB 元数据。
pub(crate) fn auto_match_library(
    state: &ApiState,
    kind: MediaKind,
    tmdb_id: Option<&str>,
    user_id: Option<UserId>,
) -> Option<Library> {
    let tmdb_id = tmdb_id.filter(|id| !id.is_empty())?;
    let snapshot_res = state.store.with_read_snapshot(
        &[store::Db::Library, store::Db::App],
        crate::http::store_wait::STORE_READ_WAIT,
        || -> Result<Vec<Library>, store::StoreError> {
            let ownership = state.store.library_ownership()?;
            let is_admin = match user_id {
                None => true,
                Some(uid) => state
                    .store
                    .user_role(uid)
                    .map(|role| role == "admin")
                    .unwrap_or(false),
            };
            Ok(ownership
                .libraries()
                .iter()
                .filter(|lib| {
                    lib.kind == kind
                        && !lib.match_rules.is_empty()
                        && ownership.accepts_work(&lib.id)
                        && user_can_auto_route_library(lib, user_id, is_admin)
                })
                .cloned()
                .collect())
        },
    );
    let candidates = match snapshot_res {
        Ok(Ok(candidates)) => candidates,
        Ok(Err(error)) | Err(error) => {
            tracing::warn!(%error, tmdb_id, ?kind, "自动匹配媒体库读取候选快照失败");
            return None;
        }
    };
    if candidates.is_empty() {
        return None;
    }
    let meta = match crate::scrape_metadata::fetch_tmdb_metadata(state, kind, tmdb_id) {
        Ok(Some(meta)) => meta,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(%error, tmdb_id, ?kind, "自动匹配媒体库拉取 TMDB 元数据失败");
            return None;
        }
    };
    candidates
        .into_iter()
        .find(|lib| library_match_reason(lib, &meta).is_some())
}

fn select_auto_library<'a>(
    libraries: &'a [Library],
    ownership: &store::LibraryOwnership,
    kind: MediaKind,
    meta: Option<&media::ItemMeta>,
    user_id: Option<UserId>,
    is_admin: bool,
) -> (Option<&'a Library>, bool, Option<String>) {
    let accepts = |lib: &&Library| {
        lib.kind == kind
            && ownership.accepts_work(&lib.id)
            && user_can_auto_route_library(lib, user_id, is_admin)
    };
    if let Some(meta) = meta
        && let Some((library, reason)) = libraries
            .iter()
            .filter(|lib| accepts(lib) && !lib.match_rules.is_empty())
            .find_map(|lib| library_match_reason(lib, meta).map(|reason| (lib, reason)))
    {
        return (Some(library), true, Some(reason));
    }
    (
        libraries
            .iter()
            .find(|lib| accepts(lib) && lib.is_default)
            .or_else(|| {
                libraries
                    .iter()
                    .find(|lib| accepts(lib) && lib.match_rules.is_empty())
            }),
        false,
        None,
    )
}

fn library_match_reason(library: &Library, meta: &media::ItemMeta) -> Option<String> {
    let mut matched_reasons = Vec::new();
    for rule in &library.match_rules {
        match evaluate_rule(rule, meta) {
            RuleEval::Skip => {}
            RuleEval::Match(reason) => matched_reasons.push(reason),
            RuleEval::Fail => return None,
        }
    }
    if matched_reasons.is_empty() {
        return None;
    }
    Some(format!(
        "符合「{}」收藏范围（{}）",
        library.name,
        matched_reasons.join("、")
    ))
}

fn evaluate_rule(rule: &Value, meta: &media::ItemMeta) -> RuleEval {
    let field = rule
        .get("field")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let op = rule.get("op").and_then(Value::as_str).unwrap_or_default();
    if op != "any_of" {
        return RuleEval::Skip;
    }
    let values = rule
        .get("values")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    match field {
        "genres" => match_genre_rule(&values, meta),
        "origin_countries" => match_country_rule(&values, meta),
        _ => RuleEval::Skip,
    }
}

fn match_genre_rule(values: &[Value], meta: &media::ItemMeta) -> RuleEval {
    let rule_genre_ids: Vec<i64> = values.iter().filter_map(Value::as_i64).collect();
    if rule_genre_ids.is_empty() {
        return RuleEval::Skip;
    }
    if meta.genre_ids.iter().any(|id| rule_genre_ids.contains(id)) {
        RuleEval::Match("类型吻合".into())
    } else {
        RuleEval::Fail
    }
}

fn match_country_rule(values: &[Value], meta: &media::ItemMeta) -> RuleEval {
    let rule_countries: Vec<String> = values
        .iter()
        .filter_map(|value| value.as_str().map(str::to_uppercase))
        .collect();
    if rule_countries.is_empty() {
        return RuleEval::Skip;
    }
    if meta
        .origin_countries
        .iter()
        .any(|country| rule_countries.contains(&country.to_uppercase()))
    {
        RuleEval::Match("区域吻合".into())
    } else {
        RuleEval::Fail
    }
}

fn render_preview(
    kind: MediaKind,
    body: &RoutingPreviewInput,
    snapshot: &RoutingSnapshot,
    library: Option<&Library>,
    route_matched: bool,
    route_reason: Option<String>,
) -> Response {
    let Some(library) = library else {
        return incomplete_preview(None, "还没有该类型的媒体库，先到设置里配置目录");
    };
    let Some(root) = library.root_paths.first() else {
        return incomplete_preview(
            Some(library),
            &format!("媒体库「{}」还没有根目录", library.name),
        );
    };
    let downloader = match &snapshot.requested_downloader {
        Some(Some(row)) => Some(row),
        Some(None) => {
            return err(StatusCode::NOT_FOUND, "downloader.missing", "下载器不存在");
        }
        None => snapshot.default_downloader.as_ref(),
    };
    let downloader_name = downloader.map(|row| row.name.clone());
    let entry_dir = preview_entry_dir(root, &snapshot.naming_pattern, kind, body);
    let (mode, path) = match (snapshot.watch.as_deref(), snapshot.inplace.as_deref()) {
        (Some(path), _) => ("watch", Some(path)),
        (None, Some(path)) => ("inplace", Some(path)),
        (None, None) => ("downloader_default", None),
    };
    let ok_flag = downloader.is_some();
    ok(json!({
        "mode": mode,
        "path": path,
        "entry_dir": entry_dir.clone(),
        "staging_path": if mode == "watch" { entry_dir } else { None },
        "library_id": library.id,
        "library_name": library.name,
        "default_filter_id": library.default_filter_id,
        "downloader_name": downloader_name,
        "route_matched": if route_matched { json!(true) } else { Value::Null },
        "route_reason": route_reason,
        "ok": ok_flag,
        "warning": if ok_flag { Value::Null } else { json!("还没有可用的默认下载器") },
    }))
    .into_response()
}

fn incomplete_preview(library: Option<&Library>, warning: &str) -> Response {
    ok(json!({
        "mode": "downloader_default",
        "path": Value::Null,
        "entry_dir": Value::Null,
        "staging_path": Value::Null,
        "library_id": library.map(|lib| json!(lib.id)).unwrap_or(Value::Null),
        "library_name": library.map(|lib| json!(lib.name)).unwrap_or(Value::Null),
        "downloader_name": Value::Null,
        "route_matched": Value::Null,
        "route_reason": Value::Null,
        "ok": false,
        "warning": warning,
    }))
    .into_response()
}

fn preview_entry_dir(
    root: &Path,
    pattern: &str,
    kind: MediaKind,
    body: &RoutingPreviewInput,
) -> Option<String> {
    let media = Media {
        id: MediaId::new(),
        kind,
        title: body.title.clone().unwrap_or_else(|| "未知片名".into()),
        year: body.year,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: body.tmdb_id.clone(),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let release = Release {
        title: media.title.clone(),
        year: media.year,
        season: None,
        episode: None,
        episode_to: None,
        resolution: None,
        source: None,
        codec: None,
        hdr: None,
        subtitle_language: None,
        audio_language: None,
        group: None,
        confidence: domain::Confidence::High,
        season_inferred: false,
    };
    library::render_path(
        root,
        pattern,
        &media,
        &release,
        Path::new("placeholder.mkv"),
    )
    .ok()
    .and_then(|path| path.parent().map(|dir| dir.display().to_string()))
}
