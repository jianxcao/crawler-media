//! Request observation middleware: access log + `x-request-id` echo.

use axum::response::Response;

use crate::http::request_observation;

/// 请求观测中间件。
///
/// `headers_latency_ms` 只覆盖到**响应头就绪**为止，不包含 body / SSE / 视频流的
/// 完整传输时间。慢阈值由 layer state 注入，中间件本身不读 env。
pub async fn request_logger(
    axum::extract::State(slow_after): axum::extract::State<std::time::Duration>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let query = request
        .uri()
        .query()
        .map(request_observation::redact_query)
        .filter(|query| !query.is_empty());
    // `MatchedPath` 由路由层在匹配后写入 extensions；未匹配时退回 raw path。
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|matched| matched.as_str().to_string())
        .unwrap_or_else(|| path.clone());
    let request_id = request_observation::request_id_from(request.headers());
    let user_agent = request
        .headers()
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let start = std::time::Instant::now();

    let mut response = next.run(request).await;
    let elapsed = start.elapsed();
    let status = response.status();
    let observation = request_observation::observe(
        method.as_str(),
        &route,
        &request_id,
        status.as_u16(),
        elapsed,
        slow_after,
    );
    request_observation::log_request(&observation, &path, query.as_deref(), user_agent.as_deref());
    if let Ok(value) = axum::http::HeaderValue::from_str(&observation.request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}
