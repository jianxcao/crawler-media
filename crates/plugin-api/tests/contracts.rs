use std::collections::HashSet;

use plugin_api::{HOOK_CATALOG, HookKind, SUPPORTED_HOOKS};

const EXPECTED_PHASE1_HOOKS: &[&str] = &[
    "search.before",
    "search.site.after",
    "search.after",
    "filter.after",
    "subscribe.score",
    "subscribe.select",
    "download.before",
    "download.after",
    "transfer.rename",
    "transfer.completed",
    "site.checkin.after",
];

const PHASE2_DEFERRED_HOOKS: &[&str] = &[
    "site.create.before",
    "site.update.before",
    "site.delete.before",
    "site.request.before",
    "site.response.after",
    "subscribe.create.before",
    "subscribe.run.before",
    "transfer.plan",
    "transfer.intercept",
    "upgrade.decide",
    "scrape.before",
    "playback.started",
    "playback.progress",
];

#[test]
fn supported_hooks_and_catalog_cover_exact_11_phase1_hooks_without_phase2() {
    assert_eq!(SUPPORTED_HOOKS.len(), EXPECTED_PHASE1_HOOKS.len());
    assert_eq!(HOOK_CATALOG.len(), EXPECTED_PHASE1_HOOKS.len());

    let supported: HashSet<&str> = SUPPORTED_HOOKS.iter().copied().collect();
    let expected: HashSet<&str> = EXPECTED_PHASE1_HOOKS.iter().copied().collect();
    assert_eq!(supported, expected);

    for hook in PHASE2_DEFERRED_HOOKS {
        assert!(
            !supported.contains(hook),
            "Phase 2 deferred hook `{hook}` must not be in SUPPORTED_HOOKS"
        );
    }

    for entry in HOOK_CATALOG {
        assert!(
            supported.contains(entry.id),
            "catalog id {} missing",
            entry.id
        );
        assert_eq!(entry.phase, 1);
        assert!(!entry.description.is_empty());
        match entry.id {
            "download.after" | "transfer.completed" | "site.checkin.after" => {
                assert_eq!(entry.kind, HookKind::Notify);
            }
            "search.before" | "subscribe.select" | "download.before" => {
                assert!(matches!(
                    entry.kind,
                    HookKind::Gate | HookKind::FilterOrGate
                ));
            }
            _ => assert_eq!(entry.kind, HookKind::Filter),
        }
    }
}

#[test]
fn hook_catalog_and_hook_kind_serialize_cleanly_to_json() {
    let val = serde_json::to_value(HOOK_CATALOG).unwrap();
    let arr = val.as_array().unwrap();
    assert_eq!(arr.len(), 11);
    let ids: HashSet<&str> = arr
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains("transfer.rename"));
    assert!(ids.contains("transfer.completed"));
    assert_eq!(
        serde_json::to_value(HookKind::FilterOrGate).unwrap(),
        serde_json::json!("filter_or_gate")
    );
    assert_eq!(
        serde_json::to_value(HookKind::Notify).unwrap(),
        serde_json::json!("notify")
    );
}

#[test]
fn event_payload_variants_roundtrip_serde_json() {
    use plugin_api::{
        DownloadAfterEvent, EventPayload, SiteCheckinAfterEvent, TransferCompletedEvent,
    };

    let samples = vec![
        EventPayload::None,
        EventPayload::DownloadAfter(DownloadAfterEvent {
            operation_id: "op-1".into(),
            downloader_id: Some("dl-1".into()),
            info_hash: Some("deadbeef".into()),
            outcome: "submitted".into(),
            verified_total_bytes: Some(1_048_576),
        }),
        EventPayload::TransferCompleted(TransferCompletedEvent {
            media_id: "media-1".into(),
            library_id: "lib-1".into(),
            relative_path: "Movies/Dune (2021)/Dune.mkv".into(),
            file_count: 1,
            total_bytes: 2_097_152,
        }),
        EventPayload::SiteCheckinAfter(SiteCheckinAfterEvent {
            site_id: "site-1".into(),
            action: "check-in".into(),
            state: "completed".into(),
            window_key: "2026-11-22".into(),
        }),
    ];

    for payload in samples {
        let json_val = serde_json::to_value(&payload).unwrap();
        assert!(
            json_val.is_object(),
            "EventPayload must serialize as a JSON object for SQLite CHECK(json_type='object'): {json_val}"
        );
        let decoded: EventPayload = serde_json::from_value(json_val).unwrap();
        assert_eq!(decoded, payload);
    }
}
