use api::cli::{Command, Transport, run};
use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;
use super::transport;
use crate::common::*;

#[tokio::test]
async fn cli_subscribe_prints_subscribe_id() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let created = t
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            json!({
                "name": "uhd",
                "atoms": [{ "kind": "resolution", "value": "2160p", "priority": 100 }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let filter = json_body(created).await;
    t.app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": filter["data"]["id"] }),
        ))
        .await
        .unwrap();
    let out = run(
        Command::Subscribe {
            title: "The Matrix".into(),
            kind: "movie".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("id="), "{out}");
}

#[tokio::test]
async fn cli_subscribe_list_prints_no_subscribes_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Subscribes, &t).await.unwrap();
    assert_eq!(out, "No Subscribes\n");
}

#[tokio::test]
async fn cli_subscribe_list_prints_media_title() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let created = t
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            json!({
                "name": "uhd",
                "atoms": [{ "kind": "resolution", "value": "2160p", "priority": 100 }]
            }),
        ))
        .await
        .unwrap();
    let filter = json_body(created).await;
    t.app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": filter["data"]["id"] }),
        ))
        .await
        .unwrap();
    run(
        Command::Subscribe {
            title: "The Matrix".into(),
            kind: "movie".into(),
        },
        &t,
    )
    .await
    .unwrap();
    let out = run(Command::Subscribes, &t).await.unwrap();
    assert!(out.contains("The Matrix"), "{out}");
    assert!(out.contains("movie"), "{out}");
    assert!(out.contains("search"), "{out}");
}

#[tokio::test]
async fn cli_filters_lists_seeded_default_on_fresh_store() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Filters, &t).await.unwrap();
    assert!(out.contains("默认"), "{out}");
    assert!(out.contains("default"), "{out}");
}

#[tokio::test]
async fn cli_filters_prints_filter_name() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let created = t
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            json!({
                "name": "uhd",
                "atoms": [{ "kind": "resolution", "value": "2160p", "priority": 100 }]
            }),
        ))
        .await
        .unwrap();
    let filter = json_body(created).await;
    t.app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": filter["data"]["id"] }),
        ))
        .await
        .unwrap();
    let out = run(Command::Filters, &t).await.unwrap();
    assert!(out.contains("uhd"), "{out}");
    assert!(out.contains("atoms=1"), "{out}");
    assert!(out.contains("default"), "{out}");
}

#[tokio::test]
async fn cli_filters_default_prints_id_and_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    run(
        Command::FiltersAdd {
            name: "uhd".into(),
            atoms: vec![api::cli::FilterAtomSpec {
                kind: "resolution".into(),
                value: Some("2160p".into()),
                priority: 100,
            }],
        },
        &t,
    )
    .await
    .unwrap();
    let listed = t.send("GET", "/api/v1/rule-sets", None).await.unwrap();
    let id = listed.1[0]["id"].as_str().unwrap().to_string();
    let out = run(Command::FiltersDefault { id: id.clone() }, &t)
        .await
        .unwrap();
    assert!(out.contains(&format!("default_filter_id={id}")), "{out}");
    let listed = run(Command::Filters, &t).await.unwrap();
    assert!(listed.contains("default"), "{listed}");
}

#[tokio::test]
async fn cli_filters_add_prints_name_and_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(
        Command::FiltersAdd {
            name: "uhd".into(),
            atoms: vec![api::cli::FilterAtomSpec {
                kind: "resolution".into(),
                value: Some("2160p".into()),
                priority: 100,
            }],
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("uhd"), "{out}");
    assert!(out.contains("atoms=1"), "{out}");
    let listed = run(Command::Filters, &t).await.unwrap();
    assert!(listed.contains("uhd"), "{listed}");
    assert!(listed.contains("atoms=1"), "{listed}");
}

