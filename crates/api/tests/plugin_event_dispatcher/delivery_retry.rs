//! TDD regressions for Item 6: claimed plugin event deliveries must retain retry
//! eligibility on transient store read errors (instead of dead-lettering) while still
//! dead-lettering real authorization/state rejections and capping sustained retries.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use store::plugin_events::{OutboxDb, OutboxDeliveryStatus, SeedSubscriberDeliveriesInput};

use super::{HttpEnv, WebhookStub, emit_download_after, install_notify_plugin};

#[tokio::test]
async fn claimed_delivery_transient_read_error_schedules_retry_and_recovers() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let srv = WebhookStub::start(false);
    let plugin_id = "org.example.evtretry";
    install_notify_plugin(
        &env,
        plugin_id,
        "download.after",
        &["hooks:download.after", "events:download.after", "http"],
        Some(srv.addr),
        true,
    )
    .await;

    let t0 = store::unix_now_ms().unwrap();
    for (idx, (event_id, op_id, fault_kind)) in [
        ("ev-claimed-grants-1", "op-cg-1", "grants"),
        ("ev-claimed-plugin-1", "op-cp-1", "plugin"),
    ]
    .into_iter()
    .enumerate()
    {
        let base_t = t0 + (idx as i64) * 10_000;
        emit_download_after(&env.state.store(), event_id, op_id, base_t);
        env.state
            .store()
            .seed_event_fanout(
                OutboxDb::Subscribe,
                &SeedSubscriberDeliveriesInput {
                    event_id: event_id.into(),
                    plugin_ids: vec![plugin_id.into()],
                    next_attempt_at: base_t,
                    now_ms: base_t,
                },
            )
            .unwrap();

        env.state
            .plugins()
            .set_fanout_read_fault_for_test(Some(Arc::new(move |op| match (fault_kind, op) {
                (
                    "grants",
                    api::FanoutReadOp::PluginGrants {
                        plugin_id: "org.example.evtretry",
                    },
                ) => Err(store::StoreError::ReadBusy),
                (
                    "plugin",
                    api::FanoutReadOp::GetPlugin {
                        plugin_id: "org.example.evtretry",
                    },
                ) => Err(store::StoreError::ReadDeadline),
                _ => Ok(()),
            })));

        let hits_before = srv.hits.load(Ordering::SeqCst);
        api::drain_plugin_events_for_test(&env.state, base_t + 5).await;

        let receipt = env
            .state
            .store()
            .get_delivery_receipt(OutboxDb::Subscribe, event_id, plugin_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            receipt.status,
            OutboxDeliveryStatus::Pending,
            "transient {fault_kind} read error after claim must schedule retry, not dead_letter"
        );
        assert_eq!(receipt.attempt_count, 1);
        assert!(
            receipt.next_attempt_at > base_t + 5,
            "retry backoff must schedule next_attempt_at in the future"
        );
        assert_eq!(srv.hits.load(Ordering::SeqCst), hits_before);

        // Clear fault and advance past next_attempt_at -> must recover and deliver once.
        env.state.plugins().set_fanout_read_fault_for_test(None);
        api::drain_plugin_events_for_test(&env.state, receipt.next_attempt_at + 10).await;

        let recovered = env
            .state
            .store()
            .get_delivery_receipt(OutboxDb::Subscribe, event_id, plugin_id)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.status, OutboxDeliveryStatus::Delivered);
        assert_eq!(recovered.attempt_count, 2);
        assert_eq!(srv.hits.load(Ordering::SeqCst), hits_before + 1);
    }
}

#[tokio::test]
async fn claimed_delivery_revoked_grant_dead_letters_and_sustained_transient_caps_at_max_attempts()
{
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let srv = WebhookStub::start(false);
    let plugin_id = "org.example.evtcap";
    install_notify_plugin(
        &env,
        plugin_id,
        "download.after",
        &["hooks:download.after", "events:download.after", "http"],
        Some(srv.addr),
        true,
    )
    .await;

    let t0 = store::unix_now_ms().unwrap();

    // 1. Sustained transient read errors hit MAX_EVENT_ATTEMPTS (5) and then dead-letter.
    emit_download_after(&env.state.store(), "ev-sustained-1", "op-sus-1", t0);
    env.state
        .store()
        .seed_event_fanout(
            OutboxDb::Subscribe,
            &SeedSubscriberDeliveriesInput {
                event_id: "ev-sustained-1".into(),
                plugin_ids: vec![plugin_id.into()],
                next_attempt_at: t0,
                now_ms: t0,
            },
        )
        .unwrap();

    env.state
        .plugins()
        .set_fanout_read_fault_for_test(Some(Arc::new(|op| match op {
            api::FanoutReadOp::PluginGrants {
                plugin_id: "org.example.evtcap",
            } => Err(store::StoreError::ReadBusy),
            _ => Ok(()),
        })));

    let mut now = t0 + 1;
    for expected_attempt in 1..=4 {
        api::drain_plugin_events_for_test(&env.state, now).await;
        let r = env
            .state
            .store()
            .get_delivery_receipt(OutboxDb::Subscribe, "ev-sustained-1", plugin_id)
            .unwrap()
            .unwrap();
        assert_eq!(r.status, OutboxDeliveryStatus::Pending);
        assert_eq!(r.attempt_count, expected_attempt);
        now = r.next_attempt_at + 10;
    }
    api::drain_plugin_events_for_test(&env.state, now).await;
    let capped = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::Subscribe, "ev-sustained-1", plugin_id)
        .unwrap()
        .unwrap();
    assert_eq!(capped.status, OutboxDeliveryStatus::DeadLetter);
    assert_eq!(capped.attempt_count, 5);
    assert_eq!(srv.hits.load(Ordering::SeqCst), 0);

    // 2. Real missing grant (plugin installed without events:download.after) still dead-letters immediately.
    env.state.plugins().set_fanout_read_fault_for_test(None);
    let no_grant_id = "org.example.evtnogrant";
    install_notify_plugin(
        &env,
        no_grant_id,
        "download.after",
        &["hooks:download.after", "http"],
        Some(srv.addr),
        true,
    )
    .await;
    let t2 = now + 10_000;
    emit_download_after(&env.state.store(), "ev-nogrant-1", "op-ng-1", t2);
    env.state
        .store()
        .seed_event_fanout(
            OutboxDb::Subscribe,
            &SeedSubscriberDeliveriesInput {
                event_id: "ev-nogrant-1".into(),
                plugin_ids: vec![no_grant_id.into()],
                next_attempt_at: t2,
                now_ms: t2,
            },
        )
        .unwrap();
    api::drain_plugin_events_for_test(&env.state, t2 + 5).await;
    let rejected = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::Subscribe, "ev-nogrant-1", no_grant_id)
        .unwrap()
        .unwrap();
    assert_eq!(rejected.status, OutboxDeliveryStatus::DeadLetter);
    assert_eq!(rejected.attempt_count, 1);
    assert_eq!(srv.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn claimed_delivery_cold_cache_version_read_fault_preserves_retry_and_recovers() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let srv = WebhookStub::start(false);
    let plugin_id = "org.example.evtcold";
    install_notify_plugin(
        &env,
        plugin_id,
        "download.after",
        &["hooks:download.after", "events:download.after", "http"],
        Some(srv.addr),
        true,
    )
    .await;

    let t0 = store::unix_now_ms().unwrap();
    emit_download_after(&env.state.store(), "ev-cold-1", "op-cold-1", t0);
    env.state
        .store()
        .seed_event_fanout(
            OutboxDb::Subscribe,
            &SeedSubscriberDeliveriesInput {
                event_id: "ev-cold-1".into(),
                plugin_ids: vec![plugin_id.into()],
                next_attempt_at: t0,
                now_ms: t0,
            },
        )
        .unwrap();

    // Fail the SECOND PluginVersion read (inside cold-cache component load) with ReadBusy
    // while the first PluginVersion read in resolve_delivery_targets succeeds.
    let version_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let version_calls_c = version_calls.clone();
    env.state
        .plugins()
        .set_fanout_read_fault_for_test(Some(Arc::new(move |op| match op {
            api::FanoutReadOp::PluginVersion {
                plugin_id: "org.example.evtcold",
            } => {
                let n = version_calls_c.fetch_add(1, Ordering::SeqCst) + 1;
                if n == 2 {
                    Err(store::StoreError::ReadBusy)
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        })));

    api::drain_plugin_events_for_test(&env.state, t0 + 5).await;
    let first = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::Subscribe, "ev-cold-1", plugin_id)
        .unwrap()
        .unwrap();
    assert_ne!(
        first.status,
        OutboxDeliveryStatus::DeadLetter,
        "transient ReadBusy during cold-cache policy load must never dead-letter delivery"
    );

    // Clear fault and allow any pending retry to complete.
    env.state.plugins().set_fanout_read_fault_for_test(None);
    if first.status == OutboxDeliveryStatus::Pending {
        api::drain_plugin_events_for_test(&env.state, first.next_attempt_at + 10).await;
    }
    let final_receipt = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::Subscribe, "ev-cold-1", plugin_id)
        .unwrap()
        .unwrap();
    assert_eq!(final_receipt.status, OutboxDeliveryStatus::Delivered);
    assert_eq!(srv.hits.load(Ordering::SeqCst), 1);

    // Warm-cache second event must deliver cleanly without re-reading version on cache hit.
    let t2 = t0 + 20_000;
    emit_download_after(&env.state.store(), "ev-warm-1", "op-warm-1", t2);
    env.state
        .store()
        .seed_event_fanout(
            OutboxDb::Subscribe,
            &SeedSubscriberDeliveriesInput {
                event_id: "ev-warm-1".into(),
                plugin_ids: vec![plugin_id.into()],
                next_attempt_at: t2,
                now_ms: t2,
            },
        )
        .unwrap();
    api::drain_plugin_events_for_test(&env.state, t2 + 5).await;
    let warm = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::Subscribe, "ev-warm-1", plugin_id)
        .unwrap()
        .unwrap();
    assert_eq!(warm.status, OutboxDeliveryStatus::Delivered);
    assert_eq!(srv.hits.load(Ordering::SeqCst), 2);
}
