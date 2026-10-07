use axum::extract::State;
use axum::response::{IntoResponse, Response};
use domain::UserId;
use serde_json::{Value, json};

use crate::http::ok;
use crate::management::ApiState;

pub(crate) async fn automation_readiness(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<UserId>>,
) -> Response {
    let store = state.store.lock();
    let sites = store.list_enabled_sites().unwrap_or_default();
    let filters = store.list_filters().unwrap_or_default();
    let downloader = store.default_downloader().ok().flatten();
    let libraries: Vec<Value> = store
        .list_libraries()
        .unwrap_or_default()
        .iter()
        .filter(|library| crate::http::library::library_visible(&store, library, user_id))
        .map(|library| library_readiness(library, sites.is_empty(), downloader.is_some()))
        .collect();
    let site_check = if sites.is_empty() {
        json!({ "key": "sites", "label": "资源站点", "status": "error", "detail": "还没有启用任何站点" })
    } else {
        json!({ "key": "sites", "label": "资源站点", "status": "ok", "detail": format!("{} 个站点已启用", sites.len()) })
    };
    let error_count = libraries
        .iter()
        .filter(|lib| lib["status"] == "error")
        .count()
        + usize::from(sites.is_empty())
        + usize::from(downloader.is_none());
    let warn_count = libraries
        .iter()
        .filter(|lib| lib["status"] == "warn")
        .count();
    let status = if error_count > 0 {
        "error"
    } else if warn_count > 0 || sites.is_empty() || filters.is_empty() {
        "warn"
    } else {
        "ok"
    };
    ok(json!({
        "status": status,
        "error_count": error_count,
        "warn_count": warn_count,
        "sites_configured": !sites.is_empty(),
        "rule_sets_configured": !filters.is_empty(),
        "downloaders_configured": downloader.is_some(),
        "site_check": site_check,
        "downloader_ok": downloader.is_some(),
        "libraries": libraries,
        "issues": [],
    }))
    .into_response()
}

fn library_readiness(
    library: &crate::store::Library,
    sites_empty: bool,
    downloader_ok: bool,
) -> Value {
    let roots = library
        .root_paths
        .iter()
        .map(|path: &std::path::PathBuf| path.display().to_string())
        .collect::<Vec<_>>()
        .join("、");
    let checks = vec![
        json!({
            "key": "delivery",
            "label": "站点搜索",
            "status": if sites_empty { "error" } else { "ok" },
            "detail": if sites_empty { "还没有启用任何资源站点" } else { "站点已接入" },
        }),
        json!({
            "key": "transfer",
            "label": "转移整理",
            "status": if downloader_ok { "ok" } else { "error" },
            "detail": if downloader_ok { "下载器已配置" } else { "还没有默认下载器" },
        }),
        json!({
            "key": "storage",
            "label": "媒体库目录",
            "status": "ok",
            "detail": roots,
        }),
    ];
    let worst = if checks.iter().any(|c| c["status"] == "error") {
        "error"
    } else if checks.iter().any(|c| c["status"] == "warn") {
        "warn"
    } else {
        "ok"
    };
    json!({
        "library_id": library.id,
        "kind": library.kind.as_str(),
        "name": library.name,
        "status": worst,
        "checks": checks,
        "issues": [],
    })
}
