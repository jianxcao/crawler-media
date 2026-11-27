//! Site connectivity maintenance: verify / login / check-in.

use super::*;
use crate::http::store_wait::store_read;

mod credentials;
use credentials::AdmittedCredentials;

pub(crate) async fn verify_site(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let observed = match store_read(&state, &[store::Db::App], move |store| {
        store.observe_site(id).ok().flatten()
    })
    .await
    {
        Ok(Some(obs)) => obs,
        Ok(None) => return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在"),
        Err(error) => return error.response(),
    };
    let state_for_work = state.clone();
    let outcome = state
        .http_work
        .run(SITE_READ_DEADLINE, move || {
            verify_site_work(&state_for_work, &observed)
        })
        .await;
    let (error, torrent_count) = match outcome {
        Ok(Ok(Ok((err_opt, count)))) => (err_opt, count),
        Ok(Ok(Err(err_msg))) => (Some(err_msg), 0),
        Ok(Err(rejection_response)) => return rejection_response,
        Err(error) => return blocking_error(error, "site.verify_timeout"),
    };
    ok(json!({ "ok": error.is_none(), "error": error, "torrent_count": torrent_count }))
        .into_response()
}

fn verify_site_work(
    state: &ApiState,
    observed: &store::SiteObservation,
) -> Result<Result<(Option<String>, usize), String>, Response> {
    let site = observed.site().clone();
    match crate::site_maintenance::resolve_maintenance_provider(
        state,
        &site,
        indexer::MaintenanceAction::VerifySession,
        None,
    ) {
        Ok(_) => {
            state.require_writable()?;
            state.require_write_admitted()?;
            let res = crate::site_maintenance::execute_foreground_receipt_maintenance(
                state,
                observed,
                indexer::MaintenanceAction::VerifySession,
                None,
                store::unix_now(),
                &format!("verify:{}", site.id),
            )?;
            Ok(res
                .map(|_| (None, 0usize))
                .map_err(|e| hooks::sanitize_reason(&e.to_string())))
        }
        Err(crate::site_maintenance::MaintenanceError::Unsupported(msg)) => {
            let snap = state.indexer.snapshot();
            if snap
                .profile(&site.profile_id)
                .is_some_and(|p| crate::check_in::uses_cookie_maintenance(&p, &site))
            {
                Ok(Err(hooks::sanitize_reason(&msg)))
            } else if snap
                .profile(&site.profile_id)
                .is_some_and(|p| p.framework == indexer::Framework::Api)
            {
                let search = state.indexer.search(std::slice::from_ref(&site), " ");
                if let Some(failure) = search.failures.first() {
                    Ok(Err(hooks::sanitize_reason(&failure.error)))
                } else {
                    Ok(Ok((None, search.torrents.len())))
                }
            } else if site.profile_id.starts_with("plugin:")
                && snap
                    .indexer(&site.profile_id)
                    .is_none_or(|e| !e.descriptor().availability.is_enabled())
            {
                Ok(Err(format!("索引器 `{}` 不可用或已停用", site.profile_id)))
            } else if !site.profile_id.starts_with("plugin:")
                && snap.profile(&site.profile_id).is_none()
            {
                Ok(Err(format!("未知站点模板 `{}`", site.profile_id)))
            } else {
                use indexer::SiteFetch as _;
                let req = indexer::FetchRequest {
                    key: format!("verify:{}", site.id),
                    url: site.url.clone(),
                    method: indexer::FetchMethod::Get,
                    body: None,
                    cookie: site.cookie.clone(),
                    api_key: site.api_key.clone(),
                    proxy: site.proxy.clone(),
                    cdp_url: site.cdp_url.clone(),
                };
                Ok(state
                    .indexer
                    .fetch(&site, &req)
                    .map(|_| (None, 0))
                    .map_err(|e| hooks::sanitize_reason(&e.to_string())))
            }
        }
        Err(e) => Ok(Err(hooks::sanitize_reason(&e.to_string()))),
    }
}

#[derive(Clone, Copy)]
enum Action {
    Login,
    CheckIn,
}
impl Action {
    fn failure_code(self) -> &'static str {
        match self {
            Self::Login => "site.login_failed",
            Self::CheckIn => "site.check_in_failed",
        }
    }
    fn timeout_code(self) -> &'static str {
        match self {
            Self::Login => "site.login_timeout",
            Self::CheckIn => "site.check_in_timeout",
        }
    }
}

pub(crate) async fn site_login(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    maintain_site(state, id, Action::Login).await
}

pub(crate) async fn site_check_in(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    maintain_site(state, id, Action::CheckIn).await
}

async fn maintain_site(state: ApiState, id: String, action: Action) -> Response {
    let id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let worker = state.clone();
    let result = state
        .http_work
        .run_to_completion(move || {
            worker.require_writable()?;
            worker.require_write_admitted()?;
            let observed = {
                let store = worker.store.clone();
                store
                    .observe_site(id)
                    .ok()
                    .flatten()
                    .ok_or_else(|| err(StatusCode::NOT_FOUND, "site.missing", "站点不存在"))
            }?;
            let site = observed.site().clone();
            match action {
                Action::Login => {
                    let (plugin_id, _, _) =
                        match crate::site_maintenance::resolve_maintenance_provider(
                            &worker,
                            &site,
                            indexer::MaintenanceAction::VerifySession,
                            None,
                        ) {
                            Ok(triple) => triple,
                            Err(error) => {
                                return Err(err(
                                    StatusCode::BAD_REQUEST,
                                    "site.maintenance_unsupported",
                                    &hooks::sanitize_reason(&error.to_string()),
                                ));
                            }
                        };
                    let use_provider = plugin_id
                        != crate::plugins::builtins::BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID
                        || worker
                            .indexer
                            .snapshot()
                            .profile(&site.profile_id)
                            .is_none();
                    if use_provider {
                        return crate::site_maintenance::execute_foreground_receipt_maintenance(
                            &worker,
                            &observed,
                            indexer::MaintenanceAction::VerifySession,
                            None,
                            store::unix_now(),
                            &format!("login:{}", site.id),
                        )?
                        .map(|_| None)
                        .map_err(|error| {
                            let code = if error.is_unsupported() {
                                "site.maintenance_unsupported"
                            } else {
                                action.failure_code()
                            };
                            err(
                                StatusCode::BAD_REQUEST,
                                code,
                                &hooks::sanitize_reason(&error.to_string()),
                            )
                        });
                    }
                    let bus = hooks::HookPipeline::new();
                    let creds = AdmittedCredentials::new(&worker, observed);
                    let result =
                        crate::site_maintenance::perform_login(&worker, &site, &bus, &creds);
                    if let Some(response) = creds.take_rejection() {
                        return Err(response);
                    }
                    result.map(|()| None).map_err(|error| {
                        err(
                            StatusCode::BAD_REQUEST,
                            action.failure_code(),
                            &hooks::sanitize_reason(&error.to_string()),
                        )
                    })
                }
                Action::CheckIn => {
                    crate::site_maintenance::perform_foreground_check_in(&worker, &observed)?
                        .map(Some)
                        .map_err(|error| {
                            let code = if error.is_unsupported() {
                                "site.maintenance_unsupported"
                            } else {
                                action.failure_code()
                            };
                            err(
                                StatusCode::BAD_REQUEST,
                                code,
                                &hooks::sanitize_reason(&error.to_string()),
                            )
                        })
                }
            }
        })
        .await;
    match result {
        Ok(Ok(Some(res))) => ok(json!({
            "ok": true,
            "deduplicated": res.deduplicated,
            "status": if res.deduplicated {
                "already-completed"
            } else {
                res.receipt.outcome.as_deref().unwrap_or("completed")
            },
            "window_key": res.receipt.window_key,
            "receipt_id": res.receipt.receipt_id,
        }))
        .into_response(),
        Ok(Ok(None)) => ok(json!({ "ok": true })).into_response(),
        Ok(Err(response)) => response,
        Err(error) => blocking_error(error, action.timeout_code()),
    }
}
