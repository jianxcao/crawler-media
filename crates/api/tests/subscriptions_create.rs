#[path = "subscription_coverage_atomicity/support.rs"]
#[allow(dead_code)]
mod support;

use axum::http::StatusCode;
use serde_json::json;
use support::{Fixture, runtime};

#[test]
fn create_subscribe_rejects_tracking_state_field() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        let mut body = fixture.body(1, 5);
        body["tracking_state"] = json!("paused");
        let (status, response) = fixture.call("POST", "/api/v1/subscriptions", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        assert!(
            fixture
                .state
                .store()
                .list_all_subscribes()
                .unwrap()
                .is_empty()
        );
    });
}

#[test]
fn hr_unknown_policy_survives_create_reload_and_patch() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        let body = json!({
            "name": "Strict HR",
            "atoms": [{"kind": "hr", "priority": 100, "exclude": true, "unknown": "exclude"}]
        });
        let (status, response) = fixture.call("POST", "/api/v1/rule-sets", body).await;
        assert_eq!(status, StatusCode::CREATED, "{response}");
        assert_eq!(response["data"]["atoms"][0]["unknown"], "exclude");
        let uri = format!(
            "/api/v1/rule-sets/{}",
            response["data"]["id"].as_str().unwrap()
        );
        let (status, response) = fixture.call("GET", &uri, json!(null)).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(response["data"]["atoms"][0]["unknown"], "exclude");
        let body = json!({
            "name": "Lenient HR",
            "atoms": [{"kind": "hr", "priority": 100, "exclude": true, "unknown": "allow"}]
        });
        let (status, response) = fixture.call("PATCH", &uri, body).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(response["data"]["atoms"][0]["unknown"], "allow");
        let (_, response) = fixture.call("GET", &uri, json!(null)).await;
        assert_eq!(response["data"]["atoms"][0]["unknown"], "allow");
    });
}

#[test]
fn platform_and_team_atoms_survive_storage_round_trip() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        let atoms = json!([
            {"kind": "platform", "value": "NF", "priority": 70, "exclude": false},
            {"kind": "team", "value": "GROUP", "priority": 80, "exclude": true}
        ]);
        let (status, response) = fixture
            .call(
                "POST",
                "/api/v1/rule-sets",
                json!({
                    "name": "Platform and team", "atoms": atoms
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{response}");
        let uri = format!(
            "/api/v1/rule-sets/{}",
            response["data"]["id"].as_str().unwrap()
        );
        let (status, response) = fixture.call("GET", &uri, json!(null)).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(response["data"]["atoms"], atoms);
    });
}

#[test]
fn hr_atoms_require_an_explicit_valid_unknown_policy() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        for unknown in [None, Some(json!("strict")), Some(json!(true))] {
            let mut atom = json!({"kind": "hr", "priority": 100, "exclude": true});
            if let Some(unknown) = unknown {
                atom["unknown"] = unknown;
            }
            let (status, response) = fixture
                .call(
                    "POST",
                    "/api/v1/rule-sets",
                    json!({
                        "name": "Invalid HR", "atoms": [atom]
                    }),
                )
                .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        }
    });
}
