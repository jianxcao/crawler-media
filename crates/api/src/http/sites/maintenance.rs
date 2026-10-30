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
    let site = match store_read(&state, move |store| store.get_site(id).ok().flatten()).await {
        Ok(Some(site)) => site,
        Ok(None) => return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在"),
        Err(error) => return error.response(),
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
    ok(json!({ "ok": has_hits || error.is_none(), "error": error,
        "torrent_count": outcome.torrents.len() }))
    .into_response()
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
            let site = {
                let store = worker.store.clone();
                store
                    .get_site(id)
                    .ok()
                    .flatten()
                    .ok_or_else(|| err(StatusCode::NOT_FOUND, "site.missing", "站点不存在"))
            }?;
            let _policy = crate::check_in::site_policy(&worker, &site).map_err(|error| {
                err(
                    StatusCode::BAD_REQUEST,
                    "site.maintenance_unsupported",
                    &error.to_string(),
                )
            })?;
            let bus = hooks::Bus::new();
            let creds = AdmittedCredentials::new(&worker, site.clone());
            let result = match action {
                Action::Login => {
                    crate::site_maintenance::perform_login(&worker, &site, &bus, &creds)
                }
                Action::CheckIn => {
                    crate::site_maintenance::perform_check_in(&worker, &site, &bus, &creds)
                }
            };
            if let Some(response) = creds.take_rejection() {
                return Err(response);
            }
            result.map_err(|error| {
                err(
                    StatusCode::BAD_REQUEST,
                    action.failure_code(),
                    &error.to_string(),
                )
            })
        })
        .await;
    match result {
        Ok(Ok(())) => ok(json!({ "ok": true })).into_response(),
        Ok(Err(response)) => response,
        Err(error) => blocking_error(error, action.timeout_code()),
    }
}
