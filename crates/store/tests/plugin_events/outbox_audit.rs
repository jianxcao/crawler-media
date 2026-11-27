use domain::{MaintenanceVia, Site, SiteId};
use store::Store;
use store::plugin_events::{
    EmitOutboxEventInput, OutboxDb, OutboxDeliveryStatus, SeedSubscriberDeliveriesInput,
};
use store::plugins::{PluginEffectObservation, PluginOperationState, RecordPluginAudit};
use store::{
    ClaimSiteMaintenanceReceipt, FinalizeSiteMaintenanceReceipt, SiteMaintenanceAction,
    SiteMaintenanceClaimResult, SiteMaintenanceState,
};

use super::sample_event;

fn test_site(site_id: SiteId) -> Site {
    Site {
        id: site_id,
        name: "Example".into(),
        url: "https://example.com".into(),
        profile_id: "pterclub".into(),
        cookie: None,
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: MaintenanceVia::Http,
        browser_search: false,
        timezone: "UTC".into(),
    }
}

#[test]
fn append_plugin_audit_writes_terminal_redacted_entries_and_lists_plugin_deliveries() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    store
        .append_plugin_audit(&RecordPluginAudit {
            plugin_id: "org.example.hook".into(),
            generation: Some(2),
            action: "transfer.rename".into(),
            method: "HOOK".into(),
            path: "/hooks/transfer.rename".into(),
            cause_id: "wf-rename-1".into(),
            state: PluginOperationState::Rejected,
            phase: "rejected: path traversal ../escape.mkv".into(),
            effect: PluginEffectObservation::NoEffect,
        })
        .unwrap();

    let audit = store
        .list_plugin_audit_events(
            "org.example.hook",
            Some("transfer.rename"),
            None,
            None,
            Some(10),
        )
        .unwrap();
    assert_eq!(audit.items.len(), 1);
    assert_eq!(audit.items[0].action, "transfer.rename");
    assert_eq!(audit.items[0].state, PluginOperationState::Rejected);
    assert_eq!(audit.items[0].subject_kind, "system");

    store
        .emit_outbox_event(OutboxDb::Library, &sample_event("ev-list-1", "m-1", 1000))
        .unwrap();
    store
        .seed_event_fanout(
            OutboxDb::Library,
            &SeedSubscriberDeliveriesInput {
                event_id: "ev-list-1".into(),
                plugin_ids: vec!["org.example.hook".into()],
                next_attempt_at: 1000,
                now_ms: 1000,
            },
        )
        .unwrap();
    let all_deliveries = store
        .list_plugin_event_deliveries(OutboxDb::Library, "org.example.hook", None, None, Some(10))
        .unwrap();
    assert_eq!(all_deliveries.items.len(), 1);
    assert_eq!(
        all_deliveries.items[0].delivery.status,
        OutboxDeliveryStatus::Pending
    );
}

#[test]
fn atomic_domain_writes_with_outbox_emit_on_commit_and_leave_zero_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let site_id = SiteId::new();
    store.insert_site(&test_site(site_id)).unwrap();

    let SiteMaintenanceClaimResult::Claimed(receipt) = store
        .claim_site_maintenance_receipt(&ClaimSiteMaintenanceReceipt {
            site_id,
            action: SiteMaintenanceAction::CheckIn,
            window_key: "2026-11-23".into(),
            timezone: "UTC".into(),
            window_start_utc: 1000,
            window_end_utc: 2000,
            now_utc: 1100,
            plugin_id: "builtin.checkin".into(),
            provider_id: "builtin".into(),
            generation: 0,
            credential_epoch: "ep-1".into(),
        })
        .unwrap()
    else {
        panic!("expected claimed receipt");
    };

    let stale_outbox = EmitOutboxEventInput {
        event_id: "ev-checkin-stale".into(),
        event_domain: "site".into(),
        event_kind: "site.checkin.after".into(),
        entity_type: "site".into(),
        entity_id: site_id.to_string(),
        ..sample_event("ev-checkin-stale", &site_id.to_string(), 1200)
    };
    assert!(
        store
            .finalize_site_maintenance_receipt_with_outbox(
                &FinalizeSiteMaintenanceReceipt {
                    receipt_id: receipt.receipt_id.clone(),
                    expected_revision: receipt.revision + 99,
                    state: SiteMaintenanceState::Completed,
                    outcome: Some("completed".into()),
                    evidence: Some("ok".into()),
                    now_utc: 1200,
                },
                Some(&stale_outbox),
            )
            .is_err()
    );
    assert!(
        store
            .get_outbox_event(OutboxDb::App, "ev-checkin-stale")
            .unwrap()
            .is_none()
    );

    let ok_outbox = EmitOutboxEventInput {
        event_id: "ev-checkin-ok".into(),
        ..stale_outbox
    };
    store
        .finalize_site_maintenance_receipt_with_outbox(
            &FinalizeSiteMaintenanceReceipt {
                receipt_id: receipt.receipt_id,
                expected_revision: receipt.revision,
                state: SiteMaintenanceState::Completed,
                outcome: Some("completed".into()),
                evidence: Some("ok".into()),
                now_utc: 1200,
            },
            Some(&ok_outbox),
        )
        .unwrap();
    assert!(
        store
            .get_outbox_event(OutboxDb::App, "ev-checkin-ok")
            .unwrap()
            .is_some()
    );
}
