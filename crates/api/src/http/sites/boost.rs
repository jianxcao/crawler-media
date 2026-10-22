//! Site upload-boost stats: boost key, patch and aggregate stats.

use super::*;

fn boost_key(id: &str) -> String {
    format!("boost.{}", id)
}

pub(crate) async fn patch_boost(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<BoostInput>,
) -> Response {
    let id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let store = state.store.lock();
    if store.get_site(id).ok().flatten().is_none() {
        return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
    }
    let key = boost_key(&id.to_string());
    let current = store
        .get_setting(&key)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    let mut next = current;
    if let Some(enabled) = body.enabled {
        next["enabled"] = json!(enabled);
    }
    if let Some(budget) = body.budget_bytes {
        next["budget_bytes"] = json!(budget);
    }
    if let Some(hold) = body.hold_days {
        next["hold_days"] = json!(hold);
    }
    if let Err(error) = store.put_setting(&key, &next.to_string()) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    ok(json!({
        "site_id": id.to_string(),
        "boost": next,
    }))
    .into_response()
}

pub(crate) async fn boost_stats(State(state): State<ApiState>) -> Response {
    let (rows, category) = {
        let store = state.store.lock();
        let sites = match store.list_sites() {
            Ok(sites) => sites,
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        };
        let rows: Vec<Value> = sites
            .iter()
            .map(|site| {
                let raw = store
                    .get_setting(&boost_key(&site.id.to_string()))
                    .ok()
                    .flatten()
                    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                    .unwrap_or_else(|| json!({ "enabled": false }));
                json!({
                    "site_id": site.id.to_string(),
                    "site_name": site.name,
                    "boost": raw,
                })
            })
            .collect();
        // Live upload totals from the configured downloader (best-effort).
        let category = store
            .default_downloader()
            .ok()
            .flatten()
            .and_then(|d| d.category)
            .unwrap_or_default();
        (rows, category)
    };
    // 上传统计是 best-effort telemetry：worker 忙/超时/上游失败都按 0 处理，
    // 不让 /boost-stats 因为遥测而改变响应契约。
    let state_for_work = state.clone();
    let uploaded = match state
        .http_work
        .run(SITE_READ_DEADLINE, move || {
            state_for_work.downloader.uploaded_by_category(&category)
        })
        .await
    {
        Ok(Ok(uploaded)) => uploaded,
        Ok(Err(error)) => {
            tracing::warn!(%error, "boost 上传统计读取失败，按 0 处理");
            0
        }
        Err(error) => {
            tracing::warn!(%error, "boost 上传统计 worker 未完成，按 0 处理");
            0
        }
    };
    let mut out = rows;
    for row in &mut out {
        row["uploaded_bytes"] = json!(uploaded);
    }
    ok(Value::Array(out)).into_response()
}
