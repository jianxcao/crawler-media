use super::*;

#[tokio::test]
async fn libraries_list_two_kinds_and_items_match_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/libraries",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let libraries = body["data"].as_array().unwrap();
    assert_eq!(libraries.len(), 2);
    let kinds: Vec<&str> = libraries
        .iter()
        .map(|lib| lib["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"movie"));
    assert!(kinds.contains(&"tv"));
    assert_eq!(libraries[0]["stats"]["item_count"], 0);
}

#[tokio::test]
async fn rule_sets_lists_seeded_default() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/rule-sets",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let sets = body["data"].as_array().unwrap();
    assert!(sets.len() >= 1);
    assert!(sets.iter().any(|set| set["is_default"] == true));
}

#[tokio::test]
async fn subscriptions_list_starts_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/subscriptions",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn downloaders_list_starts_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/downloaders",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn sites_catalog_lists_supported_templates() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/sites/catalog",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let catalog = body["data"].as_array().unwrap();
    assert!(!catalog.is_empty());
    // U7：移植的 PT 站点应出现在站点配置里；demo 测试桩不列。
    let ids: Vec<&str> = catalog
        .iter()
        .filter_map(|s| s["profile_id"].as_str())
        .collect();
    assert!(ids.contains(&"chdbits"), "应含 chdbits: {ids:?}");
    assert!(
        ids.contains(&"hdsky") && ids.contains(&"ourbits"),
        "应含 hdsky/ourbits: {ids:?}"
    );
    assert!(!ids.contains(&"demo"), "demo 测试桩不应列出: {ids:?}");
}

#[tokio::test]
async fn create_and_delete_site_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(
        &app,
        "POST",
        "/api/v1/sites",
        Some("management-secret"),
        json!({
            "name": "MockPT",
            "url": "http://127.0.0.1:18090",
            "profile_id": "demo",
            "enabled": true,
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    let id = body["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(body["data"]["profile_id"], "demo");

    let list = send(
        &app,
        "GET",
        "/api/v1/sites",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    let list_body = json_body(list).await;
    assert!(
        list_body["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == id)
    );

    let del = send(
        &app,
        "DELETE",
        &format!("/api/v1/sites/{id}"),
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(del.status(), StatusCode::OK);
}

#[tokio::test]
async fn rule_sets_round_trip_extended_atoms() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let created = send(
        &app,
        "POST",
        "/api/v1/rule-sets",
        Some("management-secret"),
        json!({
            "name": "extended",
            "atoms": [
                { "kind": "resolution", "value": "2160p", "priority": 100 },
                { "kind": "min_seeders", "value": "10", "priority": 90 },
                { "kind": "size", "value": "500-2000", "priority": 80 },
                { "kind": "subtitle_language", "value": "zh", "priority": 70 },
                { "kind": "audio_language", "value": "cmn", "priority": 60 },
                { "kind": "hdr", "value": "DV", "priority": 50 },
                { "kind": "title_match", "value": "NF", "priority": 40, "exclude": true },
            ],
        }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let body = json_body(created).await;
    let atoms = body["data"]["atoms"].as_array().unwrap();
    assert_eq!(atoms.len(), 7, "扩展原子应全部保留");
    let kinds: Vec<&str> = atoms.iter().filter_map(|a| a["kind"].as_str()).collect();
    assert!(kinds.contains(&"min_seeders"));
    assert!(kinds.contains(&"size"));
    assert!(kinds.contains(&"subtitle_language"));
    assert!(kinds.contains(&"audio_language"));
    assert!(kinds.contains(&"hdr"));
    let excluded = atoms.iter().find(|a| a["value"] == "NF").unwrap();
    assert_eq!(excluded["exclude"], true, "黑名单原子应保留 exclude 标记");
    let size = atoms.iter().find(|a| a["kind"] == "size").unwrap();
    assert_eq!(size["value"], "500-2000");

    // 重新读取：序列化（maps.rs）往返后原子与 exclude 不丢。
    let id = body["data"]["id"].as_str().unwrap();
    let fetched = send(
        &app,
        "GET",
        &format!("/api/v1/rule-sets/{id}"),
        Some("management-secret"),
        Value::Null,
    )
    .await;
    let fetched = json_body(fetched).await;
    let fetched_atoms = fetched["data"]["atoms"].as_array().unwrap();
    assert_eq!(fetched_atoms.len(), 7, "持久化往返后扩展原子不丢");
    let fetched_excluded = fetched_atoms.iter().find(|a| a["value"] == "NF").unwrap();
    assert_eq!(fetched_excluded["exclude"], true);
}

#[tokio::test]
async fn set_limits_rejects_invalid_values_with_bad_request() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);

    // 负数应被拒绝
    let resp = send(
        &app,
        "PUT",
        "/api/v1/downloaders/limits",
        Some("management-secret"),
        json!({ "download_limit_bytes": -500 }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 非法字符串应被拒绝
    let resp = send(
        &app,
        "PUT",
        "/api/v1/downloaders/limits",
        Some("management-secret"),
        json!({ "download_limit_bytes": "not-a-number" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
