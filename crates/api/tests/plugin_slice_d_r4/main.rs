#[path = "../builtin_boost/support.rs"]
mod support;

mod item1_trust_scope_switch;
mod item2_component_isolation;
mod item3_grant_scope_matching;
mod item4_task_run_input;
mod item5_wal_replay_races;

use axum::http::StatusCode;
use serde_json::Value;
use store::plugins::CanonicalPluginRequest;
use support::plugin_http_support::{HttpEnv, TOKEN, req_json};

pub(crate) fn canon_req(method: &str, path: &str, tag: u8) -> CanonicalPluginRequest {
    CanonicalPluginRequest {
        method: method.into(),
        path: path.into(),
        body_digest: [tag; 32],
    }
}

pub(crate) async fn post_write(
    env: &HttpEnv,
    path: impl AsRef<str>,
    rev: i64,
    idem: impl AsRef<str>,
    body: Value,
) -> (StatusCode, Value) {
    let r = format!("\"{rev}\"");
    let req = req_json(
        "POST",
        path.as_ref(),
        Some(TOKEN),
        &[("if-match", &r), ("idempotency-key", idem.as_ref())],
        Some(body),
    );
    let (st, _, b) = env.send(req).await;
    (st, b)
}

pub(crate) async fn post_prep(
    env: &HttpEnv,
    path: impl AsRef<str>,
    body: Value,
) -> (StatusCode, Value) {
    let (st, _, b) = env
        .send(req_json(
            "POST",
            path.as_ref(),
            Some(TOKEN),
            &[],
            Some(body),
        ))
        .await;
    (st, b)
}
