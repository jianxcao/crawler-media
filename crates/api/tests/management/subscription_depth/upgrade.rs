//! Upgrade runs: the report a run leaves behind and what it applies to the Subscription.

use super::support::*;

async fn assert_upgrade_report_with_pending(app: &axum::Router, id: &str, tmp: &tempfile::TempDir) {
    seed_pending_torrent(
        tmp,
        id,
        "The.Long.Watch.S01E02.1080p.mkv",
        "https://example.com/e2.torrent",
    );

    let report3 = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/subscriptions/{id}/upgrade-runs"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(report3["data"]["counts"]["in_flight"], 0);
    assert_eq!(report3["data"]["counts"]["missing"], 1);
}

async fn assert_upgrade_reports(app: &axum::Router, id: &str, tmp: &tempfile::TempDir) {
    for legacy in [
        json!({ "rule_set_id": "x" }),
        json!({ "unknown_field": true }),
    ] {
        let rejected = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/subscriptions/{id}/upgrade-runs"),
                Some("management-secret"),
                legacy.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST, "{legacy}");
    }
    let patched = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            json!({ "selected_seasons": [1] }),
        ))
        .await
        .unwrap();
    assert_eq!(
        patched.status(),
        StatusCode::BAD_REQUEST,
        "selected_seasons is not a field"
    );
    let report = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/subscriptions/{id}/upgrade-runs"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(report["data"]["counts"]["missing"], 2);
    assert_eq!(report["data"]["counts"]["at_cutoff"], 0);
    assert!(report["data"]["summary"].as_str().unwrap().contains("缺失"));

    let store = Store::open(tmp.path().join("data")).unwrap();
    let mut facts = subscribe::SubscribeFacts::default();
    facts.upsert(
        Some(1),
        Some(1),
        subscribe::QualityFact {
            score: 80,
            path: Some("/tv/e1.mkv".into()),
        },
    );
    store
        .save_subscribe_facts(id.parse().unwrap(), &facts)
        .unwrap();

    let report2 = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/subscriptions/{id}/upgrade-runs"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(report2["data"]["counts"]["missing"], 1);
    assert_eq!(report2["data"]["counts"]["at_cutoff"], 1);
    assert_eq!(report2["data"]["counts"]["upgradable"], 0);

    assert_upgrade_report_with_pending(app, id, tmp).await;
}

#[tokio::test]
async fn upgrade_runs_report_matches_facts() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" },
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();

    assert_upgrade_reports(&app, &id, &tmp).await;
}

#[tokio::test]
async fn run_upgrade_applies_new_ruleset_and_activates_wash_cut() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);

    // 建立一个高画质洗版规则组 (目标分 100)
    let rule_res = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/rule-sets",
                Some("management-secret"),
                json!({
                    "name": "uhd-upgrade",
                    "atoms": [
                        { "kind": "wash_target", "value": "2160p", "priority": 100 },
                        { "kind": "resolution", "value": "2160p", "priority": 100 },
                    ]
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let rule_id = rule_res["data"]["id"].as_str().unwrap();

    // 创建普通订阅 (默认 wash_cut = false)
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" },
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 1 },
                    "wash_cut": false
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap();

    // 写入一个 720p（得分 50）的已有事实
    let store = Store::open(tmp.path().join("data")).unwrap();
    let mut facts = subscribe::SubscribeFacts::default();
    facts.upsert(
        Some(1),
        Some(1),
        subscribe::QualityFact {
            score: 50,
            path: Some("/fake/s01e01.mkv".into()),
        },
    );
    store
        .save_subscribe_facts(id.parse().unwrap(), &facts)
        .unwrap();
    drop(store);

    // 此时调用 upgrade-runs 并传递新的洗版规则组和激活洗版请求
    let upgrade_res = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/subscriptions/{id}/upgrade-runs"),
                Some("management-secret"),
                json!({
                    "filter_id": rule_id,
                    "wash_cut": true
                }),
            ))
            .await
            .unwrap(),
    )
    .await;

    // 检验：规则组必须被应用，且原有 50 分单元成功判定为 upgradable（而不是误判为 at_cutoff）
    assert_eq!(upgrade_res["data"]["filter_id"], rule_id);
    assert_eq!(
        upgrade_res["data"]["counts"]["upgradable"], 1,
        "在传递新规则组并开启洗版后，已有低分单元必须被标记为 upgradable 并开始洗版"
    );

    // 检验持久化：订阅的 wash_cut 和 wash_cut_filter_id 均已保存
    let updated = json_body(
        app.oneshot(request(
            "GET",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(updated["data"]["wash_cut"], true);
    assert_eq!(updated["data"]["wash_cut_filter_id"], rule_id);
}
