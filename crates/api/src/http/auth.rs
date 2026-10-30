//! Auth: login returns the user token (ADR-0003 plaintext token) and an
//! HttpOnly same-origin cookie for browser requests such as `<img>` loads.
//! Protected routes accept either the cookie or `Authorization: Bearer <token>`.
//! 管理员授权是独立的一层：`authenticate` 只解析身份，`require_admin`
//! 中间件在 admin Router 上统一 403，避免逐 handler 漏加。

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::management::ApiState;

const SESSION_COOKIE: &str = "mc_session";

pub(crate) async fn login(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    let username = body["username"].as_str().unwrap_or_default();
    let password = body["password"].as_str().unwrap_or_default();
    if username.is_empty() || password.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "auth.bad_request",
            "用户名和密码必填",
        );
    }
    let authenticated = state
        .password_login
        .authenticate(state.clone(), username.to_string(), password.to_string())
        .await;
    let (token, user) = match authenticated {
        Ok(Some(authenticated)) => authenticated,
        Ok(None) => {
            tracing::warn!(username = %username, "登录失败：用户名或密码不正确");
            return err(
                StatusCode::UNAUTHORIZED,
                "auth.invalid",
                "用户名或密码不正确",
            );
        }
        Err(crate::password_login::LoginError::Admission(response)) => return response,
        Err(crate::password_login::LoginError::Busy) => {
            tracing::warn!(username = %username, "登录并发已达上限，拒绝请求");
            let mut response = err(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth.busy",
                "登录服务繁忙，请稍后重试",
            );
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            return response;
        }
        Err(error) => {
            // DB/worker/hash 故障绝不能被表述成“密码错误”，也不签发 token。
            tracing::error!(username = %username, error = %error, "登录服务内部错误");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "auth.error",
                "登录服务暂时不可用",
            );
        }
    };
    tracing::info!(user_id = %user.id, login = %user.login, role = user.role.as_str(), "用户登录成功");
    let mut payload = ok(json!({
        "token": token.clone(),
        "user": {
            "id": user.id.to_string(),
            "login": user.login,
            "role": user.role.as_str(),
            "enabled": user.enabled,
        },
    }))
    .into_response();
    let cookie = format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax");
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        payload.headers_mut().append(header::SET_COOKIE, value);
    }
    payload
}

/// POST /auth/logout — 撤销当前会话 token（Header 或 cookie 之一命中即撤销），
/// 并清除浏览器会话 cookie。幂等：无 token / 未知 token 也返回成功。
pub(crate) async fn logout(State(state): State<ApiState>, request: Request) -> Response {
    let mut tokens = Vec::new();
    if let Some(bearer) = bearer_token(&request) {
        tokens.push(bearer);
    }
    if let Some(cookie) = cookie_token(&request) {
        tokens.push(cookie);
    }
    tokens.sort();
    tokens.dedup();

    // Review L2: 撤销会话要写 app.db，走 blocking 池而不是在执行器上拿锁。
    match auth_store_write(&state, move |store| store.delete_session_tokens(&tokens)).await {
        Ok(revoked_tokens) => tracing::debug!(revoked_tokens, "已撤销会话 token"),
        Err(response) => return response,
    }

    let mut response = ok(Value::Null).into_response();
    let clear = format!("{SESSION_COOKIE}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax");
    if let Ok(value) = HeaderValue::from_str(&clear) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

fn bearer_token(request: &Request) -> Option<String> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string)
}

fn query_token(request: &Request) -> Option<String> {
    // Inside a nested router `uri()` has the mount prefix stripped; match the
    // allow-list against the URI the client actually requested.
    let uri = request
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map_or(request.uri(), |original| &original.0);
    let path = uri.path();
    // 窄范围 query-token：仅允许视频取流、字幕取流和直读类播放资源路由
    let mut segments = path.split('/');
    let is_session_stream = matches!(
        (segments.next(), segments.next(), segments.next(), segments.next(), segments.next(), segments.next(), segments.next()),
        (Some(""), Some("api"), Some("v1"), Some("playback"), Some("sessions"), Some(id), Some("stream")) if !id.is_empty()
    ) && segments.next().is_none();
    let is_playback_resource = path.starts_with("/api/v1/playback/subtitles/")
        || is_session_stream
        || path.starts_with("/Videos/")
        || path.starts_with("/playback/subtitles/");
    if !is_playback_resource {
        return None;
    }
    uri.query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key.eq_ignore_ascii_case("api_key")
                || key.eq_ignore_ascii_case("apikey")
                || key.eq_ignore_ascii_case("token"))
            .then(|| value.to_string())
        })
    })
}

/// 从请求中提取会话 token：优先 Authorization: Bearer，其次会话 cookie，取流端点允许 query token。
fn request_token(request: &Request) -> Option<String> {
    bearer_token(request)
        .or_else(|| cookie_token(request))
        .or_else(|| query_token(request))
}

fn cookie_token(request: &Request) -> Option<String> {
    request
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|pair| {
                let pair = pair.trim();
                pair.strip_prefix(SESSION_COOKIE)?.strip_prefix('=')
            })
        })
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// 身份 / 角色查询的绝对预算。
///
/// 生产上这是一次 app.db 单行读（几十微秒），走的是**专用读连接**
/// （各数据库共享只读连接池）：长写事务期间读者读的是 WAL 的最后提交快照，
/// 不必排在进程级 `Store` 锁后面。因此 1 s 现在只兜底两种情况——读连接拿不到
/// 一致快照（WAL checkpoint / `busy_timeout` 用尽），或 blocking worker 卡住；
/// 两者都给 503 `runtime.busy` + `Retry-After`，让客户端重试，而不是重建会话。
const AUTH_READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(1);

/// 「服务繁忙，稍后重试」——**不是**身份问题。
///
/// 身份查询读不到一致快照（读连接 `busy`）或超过 [`AUTH_READ_DEADLINE`] 时，
/// 请求本身并不该被判定为凭据失败：客户端应当重试，而不是重建会话或重新登录。
/// code 用 blocking 预算同一套 `runtime.busy`，并带 `Retry-After`。
fn auth_busy_response() -> Response {
    let mut response = err(
        StatusCode::SERVICE_UNAVAILABLE,
        "runtime.busy",
        "服务繁忙，请稍后重试",
    );
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

/// `auth.error` 只留给真正的读取失败（存储错误、worker panic）。
fn auth_error_response() -> Response {
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "auth.error",
        "身份读取失败",
    )
}

/// 库浏览端点（自带请求预算）走的 Library 读阶段失败分类。
fn auth_stage_response(error: &store::LibraryReadError) -> Response {
    if crate::library_read_work::error_is_unavailable(error) {
        tracing::warn!(%error, "身份查询未在预算内完成，请求按繁忙重试");
        auth_busy_response()
    } else {
        tracing::error!(%error, "身份查询失败");
        auth_error_response()
    }
}

/// 身份读通过共享只读 Store 池，不拿 writer 锁。
///
/// 唯一的读路径：认证、管理员角色、`/auth/me` 都从这里走，因为三者读的都是 app.db
/// 的单行。预算内的失败一律交给 [`auth_stage_response`] 分类（繁忙→503，真错→500）。
async fn auth_app_read<T, F>(state: &ApiState, work: F) -> Result<T, Response>
where
    F: FnOnce(&store::App<'_>) -> Result<T, store::StoreError> + Send + 'static,
    T: Send + 'static,
{
    let deadline = std::time::Instant::now() + AUTH_READ_DEADLINE;
    crate::library_read_work::run_app_read_stage_named(state, deadline, "auth", work)
        .await
        .map_err(|error| auth_stage_response(&error))
}

/// Session revocation uses the common bounded writer and actual App admission.
async fn auth_store_write<T, F>(state: &ApiState, work: F) -> Result<T, Response>
where
    F: FnOnce(&crate::Store) -> Result<T, store::StoreError> + Send + 'static,
    T: Send + 'static,
{
    super::store_wait::store_write(state, work)
        .await?
        .map_err(|error| {
            tracing::error!(%error, "会话撤销失败");
            auth_error_response()
        })
}

/// Resolve the acting user from the bearer token (or session cookie).
///
/// Review L2: 身份查询不在执行器线程上拿 `(&store)`。库浏览端点自带请求预算
/// （`QueryTiming`），其余端点用 [`AUTH_STORE_DEADLINE`]；两条路径共用同一段
/// off-executor 查询，避免再分叉出另一条「直接拿锁」的捷径。
pub(crate) async fn authenticate(
    State(state): State<ApiState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = request_token(&request) else {
        return err(
            StatusCode::UNAUTHORIZED,
            "auth.required",
            "未登录或会话已过期",
        );
    };
    // 库浏览端点自带请求预算（`QueryTiming`），其余端点用 [`AUTH_READ_DEADLINE`]；
    // 两条路径都是同一次 app.db 读 → 同一条专用读连接，不再分叉出「直接拿锁」的捷径。
    let scoped_deadline = request
        .extensions()
        .get::<media_server::provider::QueryTiming>()
        .map(|timing| timing.deadline);
    let owned = token.clone();
    let deadline =
        scoped_deadline.unwrap_or_else(|| std::time::Instant::now() + AUTH_READ_DEADLINE);
    let lookup = crate::library_read_work::run_app_read_stage_named(
        &state,
        deadline,
        "auth",
        move |reader| reader.user_id_by_token(&owned),
    )
    .await
    .map_err(|error| auth_stage_response(&error));
    let user_id = match lookup {
        Ok(Some(user_id)) => user_id,
        Ok(None) => {
            return err(
                StatusCode::UNAUTHORIZED,
                "auth.required",
                "未登录或会话已过期",
            );
        }
        Err(response) => return response,
    };
    if scoped_deadline.is_some() {
        request
            .extensions_mut()
            .insert(store::ReadCredential::new(user_id, token.clone()));
    }
    request.extensions_mut().insert(user_id);
    request.extensions_mut().insert(token);
    // 可选身份提取：库浏览等端点对“匿名可读”与“成员可见范围”共用同一 handler。
    request.extensions_mut().insert(Some(user_id));
    next.run(request).await
}

/// 管理员授权层：必须挂在 `authenticate` 之后（内层），读取其注入的身份。
/// Self API 管理路由使用 authenticate 注入的 UserId。
///
/// Review L2: 角色查询同样不在执行器线程上拿 `(&store)`；读不到角色时
/// 区分「稍后重试」（503）与真正的内部错误（500），不再把繁忙误报成 403。
pub(crate) async fn require_admin(
    State(state): State<ApiState>,
    request: Request,
    next: Next,
) -> Response {
    let user_id = request.extensions().get::<domain::UserId>().copied();
    let Some(user_id) = user_id else {
        return err(
            StatusCode::UNAUTHORIZED,
            "auth.required",
            "未登录或会话已过期",
        );
    };
    // 读不到角色时区分「稍后重试」（503 runtime.busy）与真正的读失败（500），
    // 不再把繁忙误报成 403「你不是管理员」。
    let role = match auth_app_read(&state, move |reader| reader.user_role(user_id)).await {
        Ok(role) => role,
        Err(response) => return response,
    };
    if role == "admin" {
        next.run(request).await
    } else {
        err(
            StatusCode::FORBIDDEN,
            "auth.forbidden",
            "只有管理员可以执行该操作",
        )
    }
}

pub(crate) async fn me(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
) -> Response {
    // Review L2: 当前用户是 app.db 单行读，走 blocking 池而不是在执行器上拿锁；
    // Task 27 起它读的是专用读连接，不再排在长写事务后面。
    let user = match auth_app_read(&state, move |reader| reader.get_user(user_id)).await {
        Ok(Some(user)) => user,
        Ok(None) => return err(StatusCode::UNAUTHORIZED, "auth.required", "用户不存在"),
        Err(response) => return response,
    };
    ok(json!({
        "id": user.id.to_string(),
        "login": user.login,
        "role": user.role.as_str(),
        "enabled": user.enabled,
    }))
    .into_response()
}

#[cfg(test)]
mod tests;
