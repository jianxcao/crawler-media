use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use domain::UserId;

use crate::store::Store;
use parking_lot::Mutex;

#[derive(Clone)]
pub struct AuthUser {
    pub id: UserId,
}

/// legacy / Jellyfin 路由的身份解析：client token → AuthUser（并同步注入
/// UserId，使 `http::auth::require_admin` 这类中间件对两套路由都可用）。
pub async fn authenticate(
    State(store): State<Arc<Mutex<Store>>>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let authorization = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(token_from_authorization);
    let client_header = ["x-emby-token", "x-mediabrowser-token"]
        .into_iter()
        .find_map(|name| {
            request
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
        });
    let query_token = request.uri().query().and_then(|q| {
        q.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            if key == "api_key" || key == "token" {
                // Jellyfin/浏览器原生 video 标签通过 query 传 token
                Some(value.to_string())
            } else {
                None
            }
        })
    });
    let token = authorization
        .map(str::to_string)
        .or_else(|| client_header.map(str::to_string))
        .or(query_token);
    let Some(token) = token else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let user = store
        .lock()
        .user_by_token(&token)
        .ok()
        .flatten()
        .ok_or(StatusCode::UNAUTHORIZED)?;
    request.extensions_mut().insert(user.id);
    request.extensions_mut().insert(AuthUser { id: user.id });
    Ok(next.run(request).await)
}

fn token_from_authorization(value: &str) -> Option<&str> {
    let (scheme, credentials) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("Bearer") {
        return Some(credentials.trim());
    }
    if !scheme.eq_ignore_ascii_case("MediaBrowser") && !scheme.eq_ignore_ascii_case("Emby") {
        return None;
    }
    credentials.split(',').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("Token")
            .then(|| value.trim().trim_matches('"'))
            .filter(|token| !token.is_empty())
    })
}
