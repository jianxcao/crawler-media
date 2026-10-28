//! Users (household members). Argon2id credentials (ADR-0010) live on the
//! users table; session tokens are issued at login and stored separately.
//! 授权在路由层收口：这些端点整体挂在 admin Router 上（见 http::router），
//! 成员请求到不了这里。改密会撤销该用户的全部会话 token。

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{User, UserId, UserRole};
use serde::Deserialize;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::http::store_wait::{busy_response, store_read, store_write, with_bounded_store};
use crate::http::{err, ok};
use crate::management::ApiState;

#[derive(Deserialize)]
pub(crate) struct CreateUserBody {
    login: String,
    password: String,
}

#[derive(Deserialize)]
pub(crate) struct PatchUserBody {
    #[serde(default)]
    login: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

fn user_json(user: &User) -> Value {
    json!({
        "id": user.id.to_string(),
        "login": user.login,
        "enabled": user.enabled,
        "role": user.role.as_str(),
    })
}

pub(crate) async fn create_user(
    State(state): State<ApiState>,
    Json(body): Json<CreateUserBody>,
) -> Response {
    if body.login.trim().is_empty() || body.password.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "user.invalid", "用户名和密码必填");
    }
    let user = User {
        id: UserId::new(),
        login: body.login,
        enabled: true,
        role: UserRole::Member,
    };
    let prepared = match prepare_password(&state, body.password).await {
        Ok(prepared) => prepared,
        Err(response) => return response,
    };
    store_write(&state, move |store| {
        if let Err(error) = store.insert_user_with_prepared_password(&user, &prepared) {
            tracing::error!(%error, user_id = %user.id, "用户凭据写入失败");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        (StatusCode::CREATED, ok(user_json(&user))).into_response()
    })
    .await
    .unwrap_or_else(|response| response)
}

pub(crate) async fn list_users(
    State(state): State<ApiState>,
    axum::Extension(_user_id): axum::Extension<domain::UserId>,
) -> Response {
    store_read(&state, move |store| {
        let users = match store.list_users() {
            Ok(users) => users,
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        };
        ok(Value::Array(users.iter().map(user_json).collect())).into_response()
    })
    .await
    .unwrap_or_else(|error| error.response())
}

pub(crate) async fn update_user(
    State(state): State<ApiState>,
    axum::Extension(_user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
    Json(body): Json<PatchUserBody>,
) -> Response {
    let id = match UserId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "user.invalid", "用户 id 无效"),
    };
    let prepared = match body
        .password
        .as_deref()
        .filter(|password| !password.trim().is_empty())
    {
        Some(password) => match prepare_password(&state, password.to_string()).await {
            Ok(prepared) => Some(prepared),
            Err(response) => return response,
        },
        None => None,
    };
    store_write(&state, move |store| {
        let Some(mut user) = (match store.get_user(id) {
            Ok(user) => user,
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        }) else {
            return err(StatusCode::NOT_FOUND, "user.missing", "用户不存在");
        };
        if body.enabled == Some(false) && user.role == UserRole::Admin {
            return err(StatusCode::BAD_REQUEST, "user.protected", "不能停用管理员");
        }
        if let Some(login) = body.login.filter(|l| !l.trim().is_empty()) {
            user.login = login;
        }
        if let Some(enabled) = body.enabled {
            user.enabled = enabled;
        }
        if let Some(prepared) = prepared {
            // 原子更新用户状态、密码并撤销全部会话 token
            if let Err(error) = store.update_user_with_prepared_password(&user, &prepared) {
                tracing::error!(%error, user_id = %user.id, "更新用户凭据失败");
                return match error {
                    store::StoreError::CredentialConflict(msg) => {
                        err(StatusCode::BAD_REQUEST, "user.credential_conflict", &msg)
                    }
                    other => err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "store.error",
                        &other.to_string(),
                    ),
                };
            }
        } else if let Err(error) = store.save_user(&user) {
            tracing::error!(%error, user_id = %user.id, "保存用户信息失败");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        ok(user_json(&user)).into_response()
    })
    .await
    .unwrap_or_else(|response| response)
}

pub(crate) async fn delete_user(
    State(state): State<ApiState>,
    axum::Extension(_user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
) -> Response {
    use crate::user_removal as removal;
    let id = match UserId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "user.invalid", "用户 id 无效"),
    };
    let worker = state.clone();
    let outcome = state
        .http_work
        .run_to_completion(move || {
            let completed = with_bounded_store(worker.store(), |store| {
                worker.require_writable()?;
                worker.require_write_admitted()?;
                removal_preflight(store, id)
            })
            .ok_or_else(busy_response)??;
            if let Some(completed) = completed {
                return Ok((completed, None));
            }
            let service = removal::UserRemovalService::new(std::sync::Arc::new(worker.clone()));
            let record = service.accept(id).map_err(|error| {
                if let Err(response) = worker
                    .require_writable()
                    .and_then(|_| worker.require_write_admitted())
                {
                    return response;
                }
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "user.removal_failed",
                    &error,
                )
            })?;
            Ok((record, Some(service)))
        })
        .await;
    let (record, service) = match outcome {
        Ok(Ok(accepted)) => accepted,
        Ok(Err(response)) => return response,
        Err(error) => {
            tracing::warn!(%error, %id, "User removal could not be admitted");
            return busy_response();
        }
    };
    if let Some(service) = service {
        tokio::task::spawn_blocking(move || {
            if let Err(error) = service.continue_removal(id) {
                tracing::error!(user_id = %id, %error, "用户删除后台阶段失败，等待重试");
            }
        });
    }
    (StatusCode::ACCEPTED, ok(removal::status_dto(&record))).into_response()
}

pub(crate) async fn removal_status(
    State(state): State<ApiState>,
    Path(operation_id): Path<String>,
) -> Response {
    store_read(
        &state,
        move |store| match crate::user_removal::find_removal(store, &operation_id) {
            Ok(Some(record)) => ok(crate::user_removal::status_dto(&record)).into_response(),
            Ok(None) => err(
                StatusCode::NOT_FOUND,
                "user_removal.missing",
                "删除操作不存在",
            ),
            Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &e),
        },
    )
    .await
    .unwrap_or_else(|error| error.response())
}

async fn prepare_password(
    state: &ApiState,
    password: String,
) -> Result<store::password::PreparedPassword, Response> {
    match state
        .http_work
        .run_to_completion(move || store::password::PreparedPassword::new(&password))
        .await
    {
        Ok(Ok(prepared)) => Ok(prepared),
        Ok(Err(error)) => {
            tracing::error!(%error, "User password hashing failed");
            Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error,
            ))
        }
        Err(error) => {
            tracing::warn!(%error, "User password hashing worker unavailable");
            Err(busy_response())
        }
    }
}

fn removal_preflight(
    store: &crate::Store,
    id: UserId,
) -> Result<Option<crate::user_removal::UserRemovalRecord>, Response> {
    use crate::user_removal as removal;
    match store.get_user(id) {
        Ok(Some(user)) if user.role == UserRole::Admin => Err(err(
            StatusCode::BAD_REQUEST,
            "user.protected",
            "不能删除管理员",
        )),
        Ok(Some(_)) => Ok(None),
        Ok(None) => {
            match removal::pending_removal_of(store, id)
                .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error))?
            {
                Some(_) => Ok(None),
                None => match removal::finished_removal_of(store, id).map_err(|error| {
                    err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error)
                })? {
                    Some(done) => Ok(Some(done)),
                    None => Err(err(StatusCode::NOT_FOUND, "user.missing", "用户不存在")),
                },
            }
        }
        Err(error) => Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        )),
    }
}
