#[path = "../builtin_boost/support.rs"]
mod support;

mod item1_close_dispatch;
mod item2_multicomp_isolation;
mod item3_secret_and_enable_scopes;
mod item4_object_kind_trim;

use axum::http::StatusCode;
use domain::SiteId;
use serde_json::Value;
use sha2::{Digest, Sha256};
use store::ManualCredentialWrite;
use store::plugins::CanonicalPluginRequest;
use support::plugin_http_support::{HttpEnv, TOKEN, req_json};
use uuid::Uuid;

pub(crate) fn http_canon_req(method: &str, path: &str, body: &Value) -> CanonicalPluginRequest {
    let bytes = serde_json::to_vec(body).unwrap();
    CanonicalPluginRequest {
        method: method.into(),
        path: path.into(),
        body_digest: Sha256::digest(&bytes).into(),
    }
}

pub(crate) async fn send_write(
    env: &HttpEnv,
    method: &str,
    path: impl AsRef<str>,
    rev: i64,
    idem: impl AsRef<str>,
    body: Value,
) -> (StatusCode, Value) {
    let r = format!("\"{rev}\"");
    let req = req_json(
        method,
        path.as_ref(),
        Some(TOKEN),
        &[("if-match", &r), ("idempotency-key", idem.as_ref())],
        Some(body),
    );
    let (st, _, b) = env.send(req).await;
    (st, b)
}

pub(crate) async fn get_json(env: &HttpEnv, path: impl AsRef<str>) -> (StatusCode, Value) {
    let (st, _, b) = env
        .send(req_json("GET", path.as_ref(), Some(TOKEN), &[], None))
        .await;
    (st, b)
}

pub(crate) async fn post_plain(
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

pub(crate) fn rotate_site_credentials(store: &store::Store, site_id: SiteId) {
    let mut site = store.observe_site(site_id).unwrap().unwrap().site().clone();
    site.cookie = Some(format!("uid=1; pass=rotated-{}", Uuid::new_v4().simple()));
    store
        .save_site(&site, ManualCredentialWrite::Replace)
        .unwrap();
}
