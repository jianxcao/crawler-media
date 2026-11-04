//! Observe protocol ingress without logging request content or concrete paths.
use axum::{extract::Request, middleware::Next, response::Response};
use std::time::Instant;

pub(super) async fn observe(request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if path == "/api/v1" || path.starts_with("/api/v1/") {
        return next.run(request).await;
    }

    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|path| path.as_str().to_owned())
        .unwrap_or_else(|| "unmatched".to_owned());
    let request_id = crate::http::request_observation::request_id_from(request.headers());
    let start = Instant::now();
    tracing::debug!(%method, %route, %request_id, "protocol request arrival");
    let response = next.run(request).await;
    let status = response.status().as_u16();
    // This ends at response headers, not at completion of a streamed body.
    let headers_latency_ms = start.elapsed().as_millis();
    if status >= 500 {
        tracing::error!(%method, %route, %request_id, status, headers_latency_ms, "protocol request complete");
    } else if status >= 400 {
        tracing::warn!(%method, %route, %request_id, status, headers_latency_ms, "protocol request complete");
    } else {
        tracing::debug!(%method, %route, %request_id, status, headers_latency_ms, "protocol request complete");
    }
    response
}
