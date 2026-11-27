#[path = "subscribe_hooks_and_admit/idempotency_retry.rs"]
mod idempotency_retry;
#[path = "subscribe_hooks_and_admit/intent_and_subscribe_idem.rs"]
mod intent_and_subscribe_idem;
#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;
#[path = "subscribe_hooks_and_admit/remediation.rs"]
mod remediation;
#[path = "subscribe_hooks_and_admit/support.rs"]
mod support;

use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use plugin_http_support::{TOKEN, assert_ok, req_json};
use serde_json::json;
use store::{RecordSubscribeDeferInput, SubscribeDeferState};
use support::{enable_hooks_plugin, setup_subscribe_env};

async fn assert_activities_have_hooks_and_invalidated_defer(
    env: &plugin_http_support::HttpEnv,
    sub_id: domain::SubscribeId,
) {
    api::drain_plugin_events_for_test(&env.state, store::unix_now_ms().unwrap()).await;
    let (st_act, _, b_act) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/subscriptions/{sub_id}/activities"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let act_data = assert_ok(st_act, &b_act, StatusCode::OK);
    let items = act_data["items"].as_array().unwrap();
    assert!(!b_act.to_string().contains("secret-abc"));
    let hook_kinds: Vec<&str> = items
        .iter()
        .filter(|i| i["type"].as_str() == Some("hook_trace"))
        .filter_map(|i| i["kind"].as_str())
        .collect();
    for expected in [
        "subscribe.score",
        "subscribe.select",
        "download.before",
        "download.after",
    ] {
        assert!(
            hook_kinds.contains(&expected),
            "missing {expected} in {hook_kinds:?}"
        );
    }
    assert!(
        items
            .iter()
            .any(|i| i["type"].as_str() == Some("deferred_candidate")
                && i["status"].as_str() == Some("invalidated"))
    );
}

#[tokio::test]
async fn subscribe_hooks_defers_policy_revision_and_activities_trace() {
    let ctx = setup_subscribe_env("The.Matrix.1999.1080p.BluRay.x264", "matrix.torrent").await;
    enable_hooks_plugin(
        &ctx.env,
        "org.example.sub-hooks",
        &[
            "subscribe.score",
            "subscribe.select",
            "download.before",
            "download.after",
        ],
    )
    .await;

    let (st_run, _, b_run) = ctx
        .env
        .send(req_json(
            "POST",
            &format!("/api/v1/subscriptions/{}/run", ctx.sub_id),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ok(st_run, &b_run, StatusCode::OK);
    assert!(ctx.guard_checked.load(Ordering::SeqCst));
    assert_eq!(
        ctx.dl.inner.submitted()[0].options.save_path.as_deref(),
        Some("plugins/demo")
    );

    let binding = ctx
        .env
        .state
        .store()
        .subscribe_policy_binding(ctx.sub_id)
        .unwrap()
        .unwrap();
    ctx.env
        .state
        .store()
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: uuid::Uuid::new_v4().to_string(),
            subscribe_id: ctx.sub_id,
            subscribe_generation: binding.jobs_sync_generation,
            policy_revision: binding.policy_revision,
            enclosure: "https://pt.example/d.torrent?passkey=secret-abc".into(),
            info_hash: None,
            plugin_id: Some("org.example.sub-hooks".into()),
            hook_id: "download.before".into(),
            reason: "rate limit passkey=secret-abc".into(),
            next_retry_at: 0,
            now_utc: 100,
        })
        .unwrap();

    let (st_patch, _, b_patch) = ctx
        .env
        .send(req_json(
            "PATCH",
            &format!("/api/v1/subscriptions/{}", ctx.sub_id),
            Some(TOKEN),
            &[],
            Some(json!({ "search_interval_secs": 1800 })),
        ))
        .await;
    assert_ok(st_patch, &b_patch, StatusCode::OK);
    let binding_after = ctx
        .env
        .state
        .store()
        .subscribe_policy_binding(ctx.sub_id)
        .unwrap()
        .unwrap();
    assert!(binding_after.policy_revision > binding.policy_revision);
    assert_eq!(
        ctx.env
            .state
            .store()
            .list_subscribe_defers(ctx.sub_id, 10)
            .unwrap()[0]
            .state,
        SubscribeDeferState::Invalidated
    );

    assert_activities_have_hooks_and_invalidated_defer(&ctx.env, ctx.sub_id).await;
}

#[tokio::test]
async fn search_admit_and_downloaders_submit_route_through_download_service() {
    let ctx = setup_subscribe_env("The.Matrix.1999.1080p.BluRay.x264", "admit.torrent").await;
    let admit_url = ctx.srv.url("admit.torrent");
    let submit_url = ctx.srv.url("submit.torrent");

    let (st_adm, _, b_adm) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &[],
            Some(json!({
                "subscribe_id": ctx.sub_id.to_string(),
                "enclosure": admit_url
            })),
        ))
        .await;
    assert_ok(st_adm, &b_adm, StatusCode::OK);
    assert!(ctx.guard_checked.load(Ordering::SeqCst));
    assert_eq!(ctx.dl.inner.submitted().len(), 1);

    let (st_sub, _, b_sub) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &[],
            Some(json!({
                "download_url": submit_url,
                "title": "The.Matrix.1999.2160p.WEB-DL",
                "site_id": ctx.site_id.to_string(),
                "subscribe_id": ctx.sub_id.to_string(),
                "force": true
            })),
        ))
        .await;
    assert_ok(st_sub, &b_sub, StatusCode::OK);
    assert_eq!(ctx.dl.inner.submitted().len(), 2);
}
