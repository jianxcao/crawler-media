//! Users (household members). Credentials (plaintext, ADR-0003) live on the
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

use crate::http::{err, ok};
use crate::management::ApiState;
use crate::store::StoreError;

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
    let store = state.store.lock();
    if let Err(error) = store.insert_user_with_password(&user, &body.password) {
        tracing::error!(%error, user_id = %user.id, "用户凭据写入失败");
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    (StatusCode::CREATED, ok(user_json(&user))).into_response()
}

pub(crate) async fn list_users(
    State(state): State<ApiState>,
    axum::Extension(_user_id): axum::Extension<domain::UserId>,
) -> Response {
    let store = state.store.lock();
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
    let store = state.store.lock();
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
    if let Some(password) = body.password.filter(|p| !p.trim().is_empty()) {
        // 原子更新用户状态、密码并撤销全部会话 token
        if let Err(error) = store.update_user_and_password(&user, &password) {
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
}

pub(crate) async fn delete_user(
    State(state): State<ApiState>,
    axum::Extension(_user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
) -> Response {
    let id = match UserId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "user.invalid", "用户 id 无效"),
    };
    let store = state.store.lock();
    match store.get_user(id) {
        Ok(Some(user)) if user.role == UserRole::Admin => {
            return err(StatusCode::BAD_REQUEST, "user.protected", "不能删除管理员");
        }
        Ok(Some(_)) => {}
        Ok(None) => return err(StatusCode::NOT_FOUND, "user.missing", "用户不存在"),
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    }
    match store.delete_user(id) {
        Ok(true) => ok(json!({ "deleted": true })).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, "user.missing", "用户不存在"),
        Err(StoreError::Protected(_)) => {
            err(StatusCode::BAD_REQUEST, "user.protected", "不能删除管理员")
        }
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}
