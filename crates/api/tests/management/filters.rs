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

#[tokio::test]
async fn obsolete_filter_atoms_are_rejected_without_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let current = json!({
        "name": "canonical",
        "atoms": [{"kind":"hdr", "value":"DV", "priority":71, "exclude":true}],
    });
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            current,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = json_data(created).await["id"].as_str().unwrap().to_string();
    let before = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/rule-sets",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    for kind in ["platforms_block", "hdr_block", "release_group_block"] {
        for (method, uri) in [
            ("POST", "/api/v1/rule-sets".to_string()),
            ("PATCH", format!("/api/v1/rule-sets/{id}")),
        ] {
            let response = app
                .clone()
                .oneshot(request(
                    method,
                    &uri,
                    Some("management-secret"),
                    json!({
                        "name":"must-not-write",
                        "atoms":[
                            {"kind":"free", "priority":5},
                            {"kind":kind, "value":"DV", "priority":99, "exclude":true},
                        ],
                    }),
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{method} {kind}"
            );
            assert_eq!(json_error(response).await["code"], "rule_set.invalid");
            let after = json_data(
                app.clone()
                    .oneshot(request(
                        "GET",
                        "/api/v1/rule-sets",
                        Some("management-secret"),
                        Value::Null,
                    ))
                    .await
                    .unwrap(),
            )
            .await;
            assert_eq!(
                after, before,
                "invalid {method} must not change Filter facts"
            );
        }
    }
}

#[tokio::test]
async fn canonical_exclusion_atoms_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let atoms = json!([
        {"kind":"title_match", "value":"GROUP", "priority":42, "exclude":true},
        {"kind":"hdr", "value":"DV", "priority":73, "exclude":true},
    ]);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            json!({"name":"current", "atoms":atoms}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let data = json_data(response).await;
    assert_eq!(data["atoms"], atoms);
    let uri = format!("/api/v1/rule-sets/{}", data["id"].as_str().unwrap());
    let patched = app
        .clone()
        .oneshot(request(
            "PATCH",
            &uri,
            Some("management-secret"),
            json!({"name":"updated", "atoms":atoms}),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    assert_eq!(json_data(patched).await["atoms"], atoms);
    let read = app
        .oneshot(request("GET", &uri, Some("management-secret"), Value::Null))
        .await
        .unwrap();
    let read = json_data(read).await;
    assert_eq!(read["name"], "updated");
    assert_eq!(read["atoms"], atoms);
}

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

    let body = subscribe_payload("search");
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
