use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::SubscribeId;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;
fn app(tmp: &tempfile::TempDir) -> axum::Router {
    router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

#[tokio::test]
async fn filter_create_and_list_round_trips_atoms() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            json!({
                "name": "uhd",
                "atoms": [
                    { "kind": "resolution", "value": "2160p", "priority": 100 },
                    { "kind": "free", "priority": 10 }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_data(created).await;
    assert_eq!(created["name"], "uhd");
    assert_eq!(created["atoms"].as_array().unwrap().len(), 2);

    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/rule-sets",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_data(listed).await;
    let rows = rows.as_array().unwrap();
    let row = rows.iter().find(|row| row["name"] == "uhd").unwrap();
    assert_eq!(row["id"], created["id"]);
    assert_eq!(row["atoms"][0]["kind"], "resolution");
    assert_eq!(row["atoms"][0]["value"], "2160p");
    assert_eq!(row["atoms"][0]["priority"], 100);
}

#[tokio::test]
async fn default_filter_is_used_when_subscribe_omits_atoms() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let filter = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            json!({
                "name": "preferred",
                "atoms": [{ "kind": "resolution", "value": "1080p", "priority": 50 }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(filter.status(), StatusCode::CREATED);
    let filter = json_data(filter).await;
    let set_default = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": filter["id"] }),
        ))
        .await
        .unwrap();
    assert_eq!(set_default.status(), StatusCode::OK);
    let set_body = json_data(set_default).await;
    assert_eq!(set_body["default_rule_set_id"], filter["id"]);

    let mut body = subscribe_payload("search");
    body.as_object_mut().unwrap().remove("filter");
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_data(created).await;

    let store = Store::open(tmp.path().join("data")).unwrap();
    let subscribe = store
        .get_subscribe(SubscribeId::from_str(created["id"].as_str().unwrap()).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(subscribe.filter_id.to_string(), filter["id"]);
    assert_eq!(created["filter_id"], filter["id"]);
}
