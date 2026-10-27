//! Site connectivity maintenance: verify / login / check-in.

use super::*;

pub(crate) async fn verify_site(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let site = {
        let store = state.store.lock();
        let Some(site) = store.get_site(id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
        };
        site
    };
    let state_for_work = state.clone();
    let outcome = state
        .http_work
        .run(SITE_READ_DEADLINE, move || {
            state_for_work
                .indexer
                .search(std::slice::from_ref(&site), " ")
        })
        .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => return blocking_error(error, "site.verify_timeout"),
    };
    let has_hits = !outcome.torrents.is_empty();
    let error = outcome
        .failures
        .into_iter()
        .next()
        .map(|failure| failure.error)
        .filter(|_| !has_hits);
    ok(json!({
        "ok": has_hits || error.is_none(),
        "error": error,
        "torrent_count": outcome.torrents.len(),
    }))
    .into_response()
}

pub(crate) async fn site_login(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let site_id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let Some(site) = state.store.lock().get_site(site_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
    };
    // 借用 adapter（StoreSites / AttendanceHttp / LoginPlugin）在闭包内构造，
    // 避免把没有 Send+Sync 约束的 HttpPost trait object 送进 spawn。
    let state_for_work = state.clone();
    let result = state
        .http_work
        .run_to_completion(move || -> Result<(), (StatusCode, &'static str, String)> {
            let _policy = crate::check_in::site_policy(&state_for_work, &site).map_err(|error| {
                (
                    StatusCode::BAD_REQUEST,
                    "site.maintenance_unsupported",
                    error.to_string(),
                )
            })?;
            let bus = hooks::Bus::new();
            let creds = crate::check_in::store_sites(&state_for_work.store);
            crate::site_maintenance::perform_login(&state_for_work, &site, &bus, &creds).map_err(
                |error| {
                    (
                        StatusCode::BAD_REQUEST,
                        "site.login_failed",
                        error.to_string(),
                    )
                },
            )
        })
        .await;
    match result {
        Ok(Ok(())) => ok(json!({ "ok": true })).into_response(),
        Ok(Err((status, code, message))) => err(status, code, &message),
        Err(error) => blocking_error(error, "site.login_timeout"),
    }
}

pub(crate) async fn site_check_in(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let site_id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let Some(site) = state.store.lock().get_site(site_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
    };
    let state_for_work = state.clone();
    let result = state
        .http_work
        .run_to_completion(move || -> Result<(), (StatusCode, &'static str, String)> {
            let _policy = crate::check_in::site_policy(&state_for_work, &site).map_err(|error| {
                (
                    StatusCode::BAD_REQUEST,
                    "site.maintenance_unsupported",
                    error.to_string(),
                )
            })?;
            let bus = hooks::Bus::new();
            let creds = crate::check_in::store_sites(&state_for_work.store);
            crate::site_maintenance::perform_check_in(&state_for_work, &site, &bus, &creds).map_err(
                |error| {
                    (
                        StatusCode::BAD_REQUEST,
                        "site.check_in_failed",
                        error.to_string(),
                    )
                },
            )
        })
        .await;
    match result {
        Ok(Ok(())) => ok(json!({ "ok": true })).into_response(),
        Ok(Err((status, code, message))) => err(status, code, &message),
        Err(error) => blocking_error(error, "site.check_in_timeout"),
    }
}
