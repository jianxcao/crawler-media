use domain::{MaintenanceVia, Site, SiteId};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Barrier, mpsc};
use store::plugins::PluginError;
use store::{
    ClaimSiteMaintenanceReceipt, Db, FinalizeSiteMaintenanceReceipt, ManualCredentialWrite,
    ResolveSiteMaintenanceReceipt, SiteMaintenanceAction, SiteMaintenanceClaimResult,
    SiteMaintenanceReceiptFilter, SiteMaintenanceState, Store, StoreError,
};

fn test_site(name: &str) -> Site {
    Site {
        id: SiteId::new(),
        name: name.into(),
        url: "https://tracker.invalid/".into(),
        profile_id: "nexusphp".into(),
        cookie: Some("uid=1; pass=secret".into()),
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: Some(12),
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: MaintenanceVia::Http,
        browser_search: false,
        timezone: "UTC".into(),
    }
}

fn app_hash(dir: &std::path::Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(dir.join("app.db")).unwrap()).to_vec()
}

fn claim_req(
    site_id: SiteId,
    action: SiteMaintenanceAction,
    (window_key, timezone, window_start_utc, window_end_utc, now_utc): (&str, &str, i64, i64, i64),
    (plugin_id, provider_id, generation, credential_epoch): (&str, &str, i64, &str),
) -> ClaimSiteMaintenanceReceipt {
    ClaimSiteMaintenanceReceipt {
        site_id,
        action,
        window_key: window_key.into(),
        timezone: timezone.into(),
        window_start_utc,
        window_end_utc,
        now_utc,
        plugin_id: plugin_id.into(),
        provider_id: provider_id.into(),
        generation,
        credential_epoch: credential_epoch.into(),
    }
}

#[test]
fn receipt_schema_parity_rejects_missing_or_extra_receipt_objects_and_prior_sites() {
    for bad_sql in [
        "DROP INDEX site_maintenance_receipts_site_window",
        "DROP INDEX site_maintenance_receipts_unresolved",
        "DROP TABLE site_maintenance_receipts",
    ] {
        let dir = tempfile::tempdir().unwrap();
        drop(Store::open(dir.path()).unwrap());
        let conn = Connection::open(dir.path().join("app.db")).unwrap();
        conn.execute_batch(bad_sql).unwrap();
        drop(conn);
        let before = app_hash(dir.path());
        assert!(
            matches!(Store::open(dir.path()), Err(StoreError::Invalid(_))),
            "expected refusal for {bad_sql}"
        );
        assert_eq!(app_hash(dir.path()), before);
    }

    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let conn = Connection::open(dir.path().join("app.db")).unwrap();
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='sites'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let prior = sql.replace(
        ",browser_search INTEGER NOT NULL DEFAULT 0 CHECK(browser_search IN (0,1)),timezone TEXT NOT NULL DEFAULT 'UTC' CHECK(length(timezone)>0)",
        "",
    );
    assert_ne!(
        prior, sql,
        "sites DDL must include browser_search & timezone"
    );
    conn.execute_batch("PRAGMA writable_schema=ON").unwrap();
    conn.execute(
        "UPDATE sqlite_schema SET sql=?1 WHERE name='sites'",
        [&prior],
    )
    .unwrap();
    conn.execute_batch("PRAGMA writable_schema=OFF").unwrap();
    drop(conn);
    let before = app_hash(dir.path());
    assert!(matches!(
        Store::open(dir.path()),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(app_hash(dir.path()), before);
}

#[test]
fn claim_site_maintenance_receipt_blocks_across_providers_credential_epoch_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let site = test_site("site-a");
    store.insert_site(&site).unwrap();
    let obs1 = store.observe_site(site.id).unwrap().unwrap();
    let epoch1 = format!("{:?}", obs1.credential_epoch());

    let t_start = 1_700_006_400; // 2023-11-15T00:00:00Z
    let t_end = t_start + 86_400;
    let first = store
        .claim_site_maintenance_receipt(&claim_req(
            site.id,
            SiteMaintenanceAction::CheckIn,
            ("2023-11-15", "UTC", t_start, t_end, t_start + 60),
            ("builtin.checkin", "nexusphp-checkin", 1, &epoch1),
        ))
        .unwrap();
    let claimed = match first {
        SiteMaintenanceClaimResult::Claimed(r) => r,
        other => panic!("expected Claimed, got {other:?}"),
    };
    assert_eq!(claimed.state, SiteMaintenanceState::Pending);
    assert_eq!(claimed.revision, 1);

    let completed = store
        .finalize_site_maintenance_receipt(&FinalizeSiteMaintenanceReceipt {
            receipt_id: claimed.receipt_id.clone(),
            expected_revision: 1,
            state: SiteMaintenanceState::Completed,
            outcome: Some("checked-in: +100 bonus".into()),
            evidence: Some("http 200".into()),
            now_utc: t_start + 62,
        })
        .unwrap();
    assert_eq!(completed.state, SiteMaintenanceState::Completed);
    assert_eq!(completed.revision, 2);

    // Different plugin & provider, same (site_id, action, window_key) -> AlreadyCompleted
    let mut updated_site = site.clone();
    updated_site.cookie = Some("uid=1; pass=rotated".into());
    store
        .save_site(&updated_site, ManualCredentialWrite::Replace)
        .unwrap();
    let obs2 = store.observe_site(site.id).unwrap().unwrap();
    let epoch2 = format!("{:?}", obs2.credential_epoch());
    assert_ne!(epoch1, epoch2);
    drop(store);

    let reopened = Store::open(dir.path()).unwrap();
    let second = reopened
        .claim_site_maintenance_receipt(&claim_req(
            site.id,
            SiteMaintenanceAction::CheckIn,
            ("2023-11-15", "UTC", t_start, t_end, t_start + 120),
            ("org.thirdparty.checkin", "alt-provider", 7, &epoch2),
        ))
        .unwrap();
    assert_eq!(
        second,
        SiteMaintenanceClaimResult::AlreadyCompleted(completed.clone())
    );

    // Verify action isolation: verify-session in the same window is separate from check-in
    let verify_claim = reopened
        .claim_site_maintenance_receipt(&claim_req(
            site.id,
            SiteMaintenanceAction::VerifySession,
            ("2023-11-15", "UTC", t_start, t_end, t_start + 125),
            (
                "builtin.nexusphp-maintenance",
                "nexusphp-verify",
                1,
                &epoch2,
            ),
        ))
        .unwrap();
    assert!(matches!(
        verify_claim,
        SiteMaintenanceClaimResult::Claimed(_)
    ));
}

#[test]
fn unknown_receipt_blocks_all_providers_until_explicitly_resolved() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let site = test_site("site-unknown");
    store.insert_site(&site).unwrap();

    let t_start = 1_700_006_400;
    let t_end = t_start + 86_400;
    let claimed = match store
        .claim_site_maintenance_receipt(&claim_req(
            site.id,
            SiteMaintenanceAction::CheckIn,
            ("2023-11-15", "UTC", t_start, t_end, t_start + 10),
            ("builtin.checkin", "nexusphp", 1, "epoch-1"),
        ))
        .unwrap()
    {
        SiteMaintenanceClaimResult::Claimed(r) => r,
        other => panic!("expected Claimed, got {other:?}"),
    };

    let unknown = store
        .mark_site_maintenance_receipt_unknown(
            &claimed.receipt_id,
            claimed.revision,
            Some("post-submit disconnect"),
            Some("socket EOF after POST"),
            t_start + 12,
        )
        .unwrap();
    assert_eq!(unknown.state, SiteMaintenanceState::Unknown);
    assert_eq!(unknown.revision, 2);

    // Survives reopen & credential epoch change; blocks other provider
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    let blocked = reopened
        .claim_site_maintenance_receipt(&claim_req(
            site.id,
            SiteMaintenanceAction::CheckIn,
            ("2023-11-15", "UTC", t_start, t_end, t_start + 300),
            ("org.other.checkin", "other-provider", 2, "epoch-2"),
        ))
        .unwrap();
    assert_eq!(
        blocked,
        SiteMaintenanceClaimResult::BlockedUnknown(unknown.clone())
    );

    // Stale CAS on resolve fails
    assert!(matches!(
        reopened.resolve_site_maintenance_receipt(&ResolveSiteMaintenanceReceipt {
            receipt_id: unknown.receipt_id.clone(),
            expected_revision: 1,
            resolved_state: SiteMaintenanceState::Failed,
            outcome: Some("confirmed no check-in".into()),
            evidence: None,
            now_utc: t_start + 305,
        }),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));

    // Resolve Unknown -> Failed allows re-claim in place
    let resolved_failed = reopened
        .resolve_site_maintenance_receipt(&ResolveSiteMaintenanceReceipt {
            receipt_id: unknown.receipt_id.clone(),
            expected_revision: 2,
            resolved_state: SiteMaintenanceState::Failed,
            outcome: Some("confirmed no effect".into()),
            evidence: Some("history page shows unchecked".into()),
            now_utc: t_start + 310,
        })
        .unwrap();
    assert_eq!(resolved_failed.state, SiteMaintenanceState::Failed);
    assert_eq!(resolved_failed.revision, 3);

    let reclaimed = match reopened
        .claim_site_maintenance_receipt(&claim_req(
            site.id,
            SiteMaintenanceAction::CheckIn,
            ("2023-11-15", "UTC", t_start, t_end, t_start + 320),
            ("org.other.checkin", "other-provider", 2, "epoch-2"),
        ))
        .unwrap()
    {
        SiteMaintenanceClaimResult::Claimed(r) => r,
        other => panic!("expected re-claim Claimed, got {other:?}"),
    };
    assert_eq!(reclaimed.receipt_id, unknown.receipt_id);
    assert_eq!(reclaimed.revision, 4);
    assert_eq!(reclaimed.state, SiteMaintenanceState::Pending);
    assert_eq!(reclaimed.plugin_id, "org.other.checkin");
}

#[test]
fn timezone_change_does_not_open_overlapping_window_before_confirmed_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let site = test_site("site-tz");
    store.insert_site(&site).unwrap();

    // UTC day 2023-11-15: [Nov 15 00:00Z, Nov 16 00:00Z)
    let utc_start = 1_700_006_400;
    let utc_end = utc_start + 86_400;
    let claimed = match store
        .claim_site_maintenance_receipt(&claim_req(
            site.id,
            SiteMaintenanceAction::CheckIn,
            ("2023-11-15", "UTC", utc_start, utc_end, utc_start + 3600),
            ("builtin.checkin", "nexusphp", 1, "ep-1"),
        ))
        .unwrap()
    {
        SiteMaintenanceClaimResult::Claimed(r) => r,
        other => panic!("expected Claimed, got {other:?}"),
    };
    let completed = store
        .finalize_site_maintenance_receipt(&FinalizeSiteMaintenanceReceipt {
            receipt_id: claimed.receipt_id.clone(),
            expected_revision: 1,
            state: SiteMaintenanceState::Completed,
            outcome: Some("done".into()),
            evidence: None,
            now_utc: utc_start + 3605,
        })
        .unwrap();

    // At 2023-11-15T18:00Z, Site timezone changes to UTC+8 where local date is 2023-11-16
    // ([Nov 15 16:00Z, Nov 16 16:00Z)), which overlaps the active UTC window ending Nov 16 00:00Z.
    let now_before_boundary = utc_start + 18 * 3600;
    let utc8_start = utc_start + 16 * 3600;
    let utc8_end = utc8_start + 86_400;
    let blocked = store
        .claim_site_maintenance_receipt(&claim_req(
            site.id,
            SiteMaintenanceAction::CheckIn,
            (
                "2023-11-16",
                "Asia/Shanghai",
                utc8_start,
                utc8_end,
                now_before_boundary,
            ),
            ("builtin.checkin", "nexusphp", 1, "ep-1"),
        ))
        .unwrap();
    assert_eq!(
        blocked,
        SiteMaintenanceClaimResult::AlreadyCompleted(completed)
    );

    // Once now_utc reaches the confirmed boundary (Nov 16 00:00Z == utc_end), 2023-11-16 can claim!
    let allowed = store
        .claim_site_maintenance_receipt(&claim_req(
            site.id,
            SiteMaintenanceAction::CheckIn,
            ("2023-11-16", "Asia/Shanghai", utc8_start, utc8_end, utc_end),
            ("builtin.checkin", "nexusphp", 1, "ep-1"),
        ))
        .unwrap();
    assert!(matches!(allowed, SiteMaintenanceClaimResult::Claimed(_)));
}

#[test]
fn concurrent_claims_and_stale_cas_are_serialized_and_fenced() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let site = test_site("site-race");
    store.insert_site(&site).unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for idx in 0..2 {
        let s = store.clone();
        let b = barrier.clone();
        let site_id = site.id;
        handles.push(std::thread::spawn(move || {
            b.wait();
            s.claim_site_maintenance_receipt(&claim_req(
                site_id,
                SiteMaintenanceAction::CheckIn,
                (
                    "2023-11-15",
                    "UTC",
                    1_700_006_400,
                    1_700_092_800,
                    1_700_006_410,
                ),
                (&format!("plugin-{idx}"), "provider", 1, "ep"),
            ))
            .unwrap()
        }));
    }
    let r1 = handles.remove(0).join().unwrap();
    let r2 = handles.remove(0).join().unwrap();
    let (claimed, inflight) = match (r1, r2) {
        (SiteMaintenanceClaimResult::Claimed(c), SiteMaintenanceClaimResult::InFlight(f))
        | (SiteMaintenanceClaimResult::InFlight(f), SiteMaintenanceClaimResult::Claimed(c)) => {
            (c, f)
        }
        other => panic!("expected one Claimed and one InFlight, got {other:?}"),
    };
    assert_eq!(claimed.receipt_id, inflight.receipt_id);

    // Stale CAS fails
    assert!(matches!(
        store.finalize_site_maintenance_receipt(&FinalizeSiteMaintenanceReceipt {
            receipt_id: claimed.receipt_id.clone(),
            expected_revision: 99,
            state: SiteMaintenanceState::Completed,
            outcome: None,
            evidence: None,
            now_utc: 1_700_006_420,
        }),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));

    // Finalize succeeded, second finalize on Completed fails with Conflict
    store
        .finalize_site_maintenance_receipt(&FinalizeSiteMaintenanceReceipt {
            receipt_id: claimed.receipt_id.clone(),
            expected_revision: claimed.revision,
            state: SiteMaintenanceState::Completed,
            outcome: Some("ok".into()),
            evidence: None,
            now_utc: 1_700_006_420,
        })
        .unwrap();
    assert!(matches!(
        store.finalize_site_maintenance_receipt(&FinalizeSiteMaintenanceReceipt {
            receipt_id: claimed.receipt_id,
            expected_revision: 2,
            state: SiteMaintenanceState::Failed,
            outcome: None,
            evidence: None,
            now_utc: 1_700_006_430,
        }),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
}

#[test]
fn receipt_reads_use_reader_pool_without_blocking_on_app_writer_and_paginate() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let site = test_site("site-reads");
    store.insert_site(&site).unwrap();

    for i in 0..55 {
        let start = 1_700_000_000 + i * 86_400;
        let claim = match store
            .claim_site_maintenance_receipt(&claim_req(
                site.id,
                SiteMaintenanceAction::CheckIn,
                (
                    &format!("win-{i:03}"),
                    "UTC",
                    start,
                    start + 86_400,
                    start + 10,
                ),
                ("builtin.checkin", "nexusphp", 1, "ep"),
            ))
            .unwrap()
        {
            SiteMaintenanceClaimResult::Claimed(r) => r,
            other => panic!("expected Claimed, got {other:?}"),
        };
        if i % 2 == 0 {
            store
                .finalize_site_maintenance_receipt(&FinalizeSiteMaintenanceReceipt {
                    receipt_id: claim.receipt_id,
                    expected_revision: claim.revision,
                    state: SiteMaintenanceState::Completed,
                    outcome: Some("ok".into()),
                    evidence: None,
                    now_utc: start + 20,
                })
                .unwrap();
        }
    }

    // Hold App writer lock while performing reads via ReaderPool
    let (tx_locked, rx_locked) = mpsc::channel();
    let (tx_release, rx_release) = mpsc::channel();
    let writer_store = store.clone();
    let handle = std::thread::spawn(move || {
        writer_store
            .with_writer(Db::App, |_| {
                tx_locked.send(()).unwrap();
                rx_release.recv().unwrap();
                Ok(())
            })
            .unwrap();
    });
    rx_locked.recv().unwrap();

    let page1 = store
        .list_site_maintenance_receipts(&SiteMaintenanceReceiptFilter::default(), None, None)
        .unwrap();
    assert_eq!(page1.items.len(), 50);
    assert!(page1.next_cursor.is_some());

    let page2 = store
        .list_site_maintenance_receipts(
            &SiteMaintenanceReceiptFilter::default(),
            page1.next_cursor.as_deref(),
            Some(999),
        )
        .unwrap();
    assert_eq!(page2.items.len(), 5);
    assert_eq!(page2.next_cursor, None);

    let unresolved = store
        .list_unresolved_site_maintenance_receipts(None, None)
        .unwrap();
    assert_eq!(unresolved.items.len(), 27);
    assert!(
        store
            .get_site_maintenance_receipt(&unresolved.items[0].receipt_id)
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_site_maintenance_receipt_for_window(
                site.id,
                SiteMaintenanceAction::CheckIn,
                "win-000",
            )
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        store.list_site_maintenance_receipts(
            &SiteMaintenanceReceiptFilter::default(),
            Some("bad-cursor"),
            None,
        ),
        Err(StoreError::Plugin(PluginError::Invalid(_)))
    ));

    tx_release.send(()).unwrap();
    handle.join().unwrap();
}

#[test]
fn verify_session_reclaims_unknown_receipt_while_check_in_stays_blocked() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    let site = test_site("pt-unknown-verify");
    store.insert_site(&site).unwrap();

    let t_start = 1_700_006_400;
    let t_end = t_start + 86_400;
    for action in [
        SiteMaintenanceAction::CheckIn,
        SiteMaintenanceAction::VerifySession,
    ] {
        let claimed = match store
            .claim_site_maintenance_receipt(&claim_req(
                site.id,
                action,
                ("2023-11-15", "UTC", t_start, t_end, t_start + 10),
                ("builtin.nexusphp-maintenance", "nexusphp", 1, "epoch-1"),
            ))
            .unwrap()
        {
            SiteMaintenanceClaimResult::Claimed(r) => r,
            other => panic!("expected Claimed, got {other:?}"),
        };
        let marked = store
            .mark_site_maintenance_receipt_unknown(
                &claimed.receipt_id,
                claimed.revision,
                Some("unknown"),
                Some("timeout"),
                t_start + 12,
            )
            .unwrap();
        assert_eq!(marked.state, SiteMaintenanceState::Unknown);
    }

    // CheckIn must remain blocked so attendance is never submitted twice
    let checkin_retry = store
        .claim_site_maintenance_receipt(&claim_req(
            site.id,
            SiteMaintenanceAction::CheckIn,
            ("2023-11-15", "UTC", t_start, t_end, t_start + 20),
            ("builtin.nexusphp-maintenance", "nexusphp", 1, "epoch-2"),
        ))
        .unwrap();
    assert!(matches!(
        checkin_retry,
        SiteMaintenanceClaimResult::BlockedUnknown(_)
    ));

    // VerifySession is read-only/idempotent and reclaims its unknown receipt cleanly
    let verify_retry = store
        .claim_site_maintenance_receipt(&claim_req(
            site.id,
            SiteMaintenanceAction::VerifySession,
            ("2023-11-15", "UTC", t_start, t_end, t_start + 20),
            ("builtin.nexusphp-maintenance", "nexusphp", 1, "epoch-2"),
        ))
        .unwrap();
    match verify_retry {
        SiteMaintenanceClaimResult::Claimed(r) => {
            assert_eq!(r.state, SiteMaintenanceState::Pending);
            assert_eq!(r.credential_epoch, "epoch-2");
        }
        other => panic!("expected Claimed for VerifySession, got {other:?}"),
    }
}
