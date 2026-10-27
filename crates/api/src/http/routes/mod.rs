//! Route assembly for `/api/v1`.
//!
//! Split out of `http`: the module tree and envelope helpers stay in
//! `http`, the route tables sit next to their audience. Pure move — route
//! strings, methods, merge order and middleware nesting are unchanged.

mod admin;
mod member;

use axum::Router;
use axum::response::Response;

use crate::http::{auth, library_operations};
use crate::management::ApiState;

use admin::{admin_core_routes, admin_library_routes, admin_metadata_routes, admin_ops_routes};
use member::{
    member_discovery_routes, member_library_routes, member_playback_routes,
    member_subscriptions_routes,
};

/// Review L-9: while another instance holds the data-directory lease, data
/// writing requests answer 409 `instance.read_only`. Reads (and POST
/// previews, which only compute) stay available.
async fn write_gate(
    axum::extract::State(state): axum::extract::State<ApiState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    use axum::http::Method;
    let reads = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    if !reads && !request.uri().path().ends_with("preview") {
        // 两道独立的门，状态码不合并：
        // - 409：另一个实例持有数据目录租约（只读实例）；
        // - 503 + Retry-After：本实例正在维护（L5 写门禁），稍后重试即可。
        if let Err(response) = state.require_writable() {
            return response;
        }
        if let Err(response) = state.require_write_admitted() {
            return response;
        }
    }
    next.run(request).await
}

fn write_gated(state: &ApiState, routes: Router<ApiState>) -> Router<ApiState> {
    routes.route_layer(axum::middleware::from_fn_with_state(
        state.clone(),
        write_gate,
    ))
}

pub(super) fn member_routes(state: &ApiState) -> Router<ApiState> {
    Router::new()
        .merge(member_discovery_routes())
        .merge(write_gated(state, member_subscriptions_routes()))
        .merge(write_gated(state, member_library_routes()))
        .merge(member_playback_routes())
}

pub(super) fn admin_routes(state: &ApiState) -> Router<ApiState> {
    Router::new()
        .merge(write_gated(state, admin_core_routes()))
        // 重建索引期间拒绝并发写：必须在 handler 改动字段之前判定，
        // 也必须在只读门（write_gate）之内，否则只读模式会被 409 盖掉。
        .merge(write_gated(
            state,
            admin_library_routes().route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                library_operations::reject_conflicting_writes,
            )),
        ))
        .merge(admin_metadata_routes())
        .merge(write_gated(state, admin_ops_routes()))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_admin,
        ))
}
