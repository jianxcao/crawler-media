//! Backend compatibility branches must be gone, not merely unused-looking.
//!
//! Each case pins a removed legacy path at a public seam: a retired query key is
//! rejected by the same generic rule as any unknown key, a qBittorrent client
//! tag from an older build is not our marker, and the admin bootstrap never
//! treats the CLI bearer as an administrator password.

#[path = "management/common.rs"]
mod common;
#[path = "support/lifecycle.rs"]
#[allow(dead_code)]
mod lifecycle;

use std::collections::HashMap;
use std::io::Write;
use std::str::FromStr;
use std::sync::Arc;

use api::bootstrap_credentials::{AdminBootstrapAction, BootstrapMode, prepare_admin_credentials};
use axum::http::StatusCode;
use domain::{User, UserId, UserRole};
use lifecycle::FakeServer;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use downloader::{Downloader, QbitConfig, QbitDownloader};

fn state(tmp: &tempfile::TempDir) -> api::ApiState {
    common::state(
        tmp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
    )
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(common::request(
            "GET",
            uri,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, common::json_body(response).await)
}

/// `genres=x` is a retired spelling of `g=x`. It must be rejected by the same
/// generic unknown-key rule as a key the API never had, so there is exactly one
/// rejection path and no second list of "known legacy spellings" to maintain.
#[tokio::test]
async fn library_query_rejects_retired_key_generically() {
    let tmp = tempfile::tempdir().unwrap();
    let app = api::router(state(&tmp));
    let libraries = {
        let response = app
            .clone()
            .oneshot(common::request(
                "GET",
                "/api/v1/libraries",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        common::json_data(response).await
    };
    let library_id = libraries
        .as_array()
        .unwrap()
        .iter()
        .find(|library| library["kind"] == "movie")
        .and_then(|library| library["id"].as_str())
        .expect("default movie library")
        .to_string();

    let retired = get(
        &app,
        &format!("/api/v1/libraries/{library_id}/items?genres=x"),
    )
    .await;
    let unknown = get(
        &app,
        &format!("/api/v1/libraries/{library_id}/items?nonexistent=x"),
    )
    .await;

    assert_eq!(
        retired.0,
        StatusCode::BAD_REQUEST,
        "retired key must not be accepted: {}",
        retired.1
    );
    assert_eq!(
        unknown.0,
        StatusCode::BAD_REQUEST,
        "unknown key must not be silently ignored: {}",
        unknown.1
    );
    assert_eq!(
        retired.1["error"]["code"], unknown.1["error"]["code"],
        "one generic rejection path, not a retired-key list: retired={} unknown={}",
        retired.1, unknown.1
    );
}

/// qBittorrent reports tags verbatim. Only the constant task marker is ours; a
/// `crawler-media-<hash>` tag written by an older build must not be mistaken for
/// it, otherwise reconcile would claim a torrent it does not own.
#[test]
fn qbit_ignores_legacy_per_torrent_tag() {
    let server = FakeServer::start(|request, stream| {
        let (status, body, extra) = if request.raw.starts_with("POST /api/v2/auth/login") {
            ("200 OK", "Ok.".to_string(), "Set-Cookie: SID=test-sid\r\n")
        } else if request.raw.contains("/api/v2/torrents/info") {
            (
                "200 OK",
                r#"[{"hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":"legacy-tagged","progress":1.0,"save_path":"/downloads","state":"stalledDL","tags":"crawler-media-0f1e2d3c4b5a","size":10,"downloaded":10,"uploaded":0,"dlspeed":0,"upspeed":0}]"#.to_string(),
                "Set-Cookie: SID=test-sid\r\n",
            )
        } else {
            ("200 OK", "[]".to_string(), "")
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n{extra}\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
    });
    let client = QbitDownloader::connect(QbitConfig {
        url: server.url(),
        username: "admin".into(),
        password: "pass".into(),
        category: None,
        path_maps: vec![],
    })
    .unwrap();

    let snapshots = client.task_snapshots().unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(
        snapshots[0].tag, "",
        "a per-torrent tag from an older build is not our marker"
    );
    assert!(
        snapshots[0].owned_tags.is_empty(),
        "ownership tags are `crawler-media-owned-*`, nothing else: {:?}",
        snapshots[0].owned_tags
    );
}

/// An administrator whose stored password still equals the CLI bearer is a
/// legacy state. Bootstrap must not read the bearer as a password and must not
/// demand a re-hash/rotate path: it keeps the row and reports `Keep`.
#[test]
fn bootstrap_never_reads_cli_token_password() {
    let tmp = tempfile::tempdir().unwrap();
    let store = api::Store::open(tmp.path()).unwrap();
    let admin_id = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    store
        .insert_user(&User {
            id: admin_id,
            login: "admin".into(),
            enabled: true,
            role: UserRole::Admin,
        })
        .unwrap();
    store.set_password(admin_id, "cli-bearer-token").unwrap();

    let action =
        prepare_admin_credentials(&store, "cli-bearer-token", None, BootstrapMode::Production)
            .expect("a stored password is not re-interpreted as a CLI credential");
    assert!(
        matches!(action, AdminBootstrapAction::Keep { .. }),
        "no legacy detection path may remain: {action:?}"
    );
}
