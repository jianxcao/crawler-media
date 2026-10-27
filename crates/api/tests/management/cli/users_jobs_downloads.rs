use super::{search_transport, transport};
use crate::common::*;
use api::cli::{Command, Transport, run};
use serde_json::json;
use std::collections::HashMap;
use tower::ServiceExt;

#[tokio::test]
async fn cli_users_prints_seeded_admin() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Users, &t).await.unwrap();
    assert!(out.contains("admin"), "{out}");
}

#[tokio::test]
async fn cli_users_prints_second_login_without_token() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    t.app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": "alice", "password": "alice-token" }),
        ))
        .await
        .unwrap();
    let out = run(Command::Users, &t).await.unwrap();
    assert!(out.contains("admin"), "{out}");
    assert!(out.contains("alice"), "{out}");
    assert!(!out.contains("alice-token"), "{out}");
    assert!(!out.contains("management-secret"), "{out}");
}

#[tokio::test]
async fn cli_users_add_prints_login_and_omits_token() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(
        Command::UsersAdd {
            login: "alice".into(),
            token: "alice-token".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("login=alice"), "{out}");
    assert!(!out.contains("alice-token"), "{out}");
    let listed = run(Command::Users, &t).await.unwrap();
    assert!(listed.contains("alice"), "{listed}");
    assert!(!listed.contains("alice-token"), "{listed}");
}
#[tokio::test]
async fn cli_jobs_tick_prints_ran_and_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::JobsTick { now: Some(1) }, &t).await.unwrap();
    assert!(out.contains("ran="), "{out}");
    assert!(
        out.contains("subscribe_rss")
            || out.contains("transfer")
            || out.contains("watch_intake")
            || out.contains("scrape"),
        "{out}"
    );
}

#[tokio::test]
async fn cli_downloads_prints_no_downloads_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Downloads, &t).await.unwrap();
    assert!(out.contains("No Downloads"), "{out}");
}

#[tokio::test]
async fn cli_downloads_prints_pending_torrent_title() {
    let tmp = tempfile::tempdir().unwrap();
    let t = search_transport(&tmp, HashMap::from([("search", nexusphp())]));
    create_site(&t.app).await;
    let subscribe = create_subscribe(&t.app, "search").await;
    let searched = t
        .send("GET", "/api/v1/search/torrents?keyword=matrix", None)
        .await
        .unwrap();
    let enclosure = searched.1["items"][0]["enclosure"]
        .as_str()
        .unwrap()
        .to_string();
    run(
        Command::Admit {
            subscribe_id: subscribe["id"].as_str().unwrap().into(),
            enclosure,
        },
        &t,
    )
    .await
    .unwrap();
    let out = run(Command::Downloads, &t).await.unwrap();
    assert!(out.contains("The.Matrix.1999"), "{out}");
    assert!(out.contains("The Matrix"), "{out}");
}

#[tokio::test]
async fn cli_jobs_prints_transfer_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Jobs, &t).await.unwrap();
    assert!(out.contains("transfer"), "{out}");
    assert!(out.contains("subscribe_rss"), "{out}");
}
