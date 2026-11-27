mod boost_cleanup;
mod boost_defers;
mod ownership_delete_guards;
mod wave1_guards;

use domain::{
    Coverage, DownloaderId, FetchMode, LibraryId, Media, MediaId, MediaKind, SiteId, Subscribe,
    SubscribeId, UserId,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use store::download_operations::*;
use store::plugins::PluginError;
use store::{Store, StoreError};

pub(crate) fn subscribe_hash(dir: &std::path::Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(dir.join("subscribe.db")).unwrap()).to_vec()
}

pub(crate) fn insert_subscribe(
    store: &Store,
    id: SubscribeId,
    library_id: Option<LibraryId>,
) -> Subscribe {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Ops fixture".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        bangumi_id: None,
        anilist_id: None,
        tvdb_id: None,
    };
    store.insert_media(&media).unwrap();
    let subscribe = Subscribe {
        id,
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: store.list_filters().unwrap()[0].id,
        wash_cut: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
        keep_old_versions: false,
    };
    store.insert_subscribe(&subscribe).unwrap();
    subscribe
}

pub(crate) fn accept_input(
    op_id: &str,
    idem: &str,
    subscribe_id: Option<SubscribeId>,
    downloader_id: Option<DownloaderId>,
) -> AcceptDownloadOperationInput {
    AcceptDownloadOperationInput {
        operation_id: op_id.into(),
        idempotency_key: idem.into(),
        source_kind: "subscribe".into(),
        subscribe_id,
        plugin_id: Some("builtin.boost".into()),
        subject: "subject:1".into(),
        cause_id: "cause-1".into(),
        site_id: Some(SiteId::new()),
        downloader_id,
        endpoint_fingerprint: Some("ep-1".into()),
        enclosure: format!("https://pt.example/{op_id}.torrent"),
        ownership_tag: format!("crawler-media-owned-{op_id}"),
        info_hash: None,
        bytes_digest: None,
        verified_bytes: None,
        request_digest: format!("req-{idem}"),
        now_utc: 1_700_000_000_000,
    }
}

#[test]
fn subscribe_schema_parity_rejects_missing_or_extra_objects_and_pre_m3_subscribes() {
    for bad_sql in [
        "DROP INDEX download_operations_unresolved_idx",
        "DROP TABLE download_operations",
        "DROP TABLE download_ownership",
        "DROP TABLE boost_reservations",
        "DROP TABLE boost_task_facts",
        "DROP TABLE subscribe_deferred_candidates",
        "CREATE TABLE unexpected_future_table(id TEXT PRIMARY KEY)",
    ] {
        let dir = tempfile::tempdir().unwrap();
        drop(Store::open(dir.path()).unwrap());
        let conn = Connection::open(dir.path().join("subscribe.db")).unwrap();
        conn.execute_batch(bad_sql).unwrap();
        drop(conn);
        let before = subscribe_hash(dir.path());
        assert!(
            matches!(Store::open(dir.path()), Err(StoreError::LayoutMismatch(_))),
            "expected LayoutMismatch for {bad_sql}"
        );
        assert_eq!(subscribe_hash(dir.path()), before);
    }

    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let conn = Connection::open(dir.path().join("subscribe.db")).unwrap();
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='subscribes'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let prior = sql.replace(
        ",policy_revision INTEGER NOT NULL DEFAULT 1 CHECK(policy_revision>0)",
        "",
    );
    assert_ne!(prior, sql, "subscribes DDL must include policy_revision");
    conn.execute_batch("PRAGMA writable_schema=ON").unwrap();
    conn.execute(
        "UPDATE sqlite_schema SET sql=?1 WHERE name='subscribes'",
        [&prior],
    )
    .unwrap();
    conn.execute_batch("PRAGMA writable_schema=OFF").unwrap();
    drop(conn);
    let before = subscribe_hash(dir.path());
    assert!(matches!(
        Store::open(dir.path()),
        Err(StoreError::LayoutMismatch(_))
    ));
    assert_eq!(subscribe_hash(dir.path()), before);
}

#[test]
fn subscribe_schema_checks_enforce_invariants() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let conn = Connection::open(dir.path().join("subscribe.db")).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    conn.execute(
        "INSERT INTO download_operations(operation_id,idempotency_key,source_kind,subject,cause_id,downloader_id,enclosure,ownership_tag,request_digest,state,created_at,updated_at)
         VALUES('op1','ik1','boost','sub','c1','dl1','https://e/1','tag1','dig1','intent',1,1)",
        [],
    )
    .unwrap();
    // final reservation without bytes_digest must fail CHECK
    assert!(
        conn.execute(
            "INSERT INTO boost_reservations(reservation_id,operation_id,quota_key,plugin_id,downloader_id,bytes_digest,reserved_bytes,stage,created_at,updated_at)
             VALUES('r1','op1','qk','p1','dl1',NULL,100,'final',1,1)",
            [],
        )
        .is_err()
    );
    // simultaneous occupied_bytes > 0 and retained_bytes > 0 must fail CHECK
    conn.execute(
        "INSERT INTO download_ownership(downloader_id,ownership_tag,operation_id,subject,enclosure,ownership_state,provenance_kind,created_at,updated_at)
         VALUES('dl1','tag1','op1','sub','https://e/1','owned','created_fresh',1,1)",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO boost_task_facts(task_fact_id,plugin_id,quota_key,downloader_id,ownership_tag,info_hash,state,occupied_bytes,retained_bytes,observed_at,created_at,updated_at)
             VALUES('tf1','p1','qk','dl1','tag1','abcd','downloading',10,10,1,1,1)",
            [],
        )
        .is_err()
    );
    // attempt_count > 10 must fail CHECK
    conn.execute(
        "INSERT INTO subscribes(id,user_id,media_id,coverage_kind,fetch_mode,filter_id,created_at,updated_at,jobs_sync_generation,lifecycle_epoch)
         VALUES('s1','u1','m1','movie','search','f1',1,1,'gen1',1)",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO subscribe_deferred_candidates(defer_id,subscribe_id,subscribe_generation,policy_revision,enclosure,hook_id,reason,attempt_count,first_deferred_at,next_retry_at,max_deadline_at,state,created_at,updated_at)
             VALUES('d1','s1','gen1',1,'https://e/1','download.before','wait',11,100,200,1000,'deferred',100,100)",
            [],
        )
        .is_err()
    );
}

#[test]
fn download_operation_idempotency_and_nullable_subscribe_survival() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    let sub_id = SubscribeId::new();
    insert_subscribe(&store, sub_id, None);

    // Nullable subscribe_id (boost/manual) works.
    let manual = store
        .accept_download_operation(&accept_input("op-manual", "idem-manual", None, Some(dl_id)))
        .unwrap();
    assert!(!manual.replayed);
    assert_eq!(manual.operation.subscribe_id, None);

    // Idempotent replay vs conflict.
    let first = store
        .accept_download_operation(&accept_input(
            "op-sub",
            "idem-sub",
            Some(sub_id),
            Some(dl_id),
        ))
        .unwrap();
    assert!(!first.replayed);
    let replay = store
        .accept_download_operation(&accept_input(
            "op-sub-2",
            "idem-sub",
            Some(sub_id),
            Some(dl_id),
        ))
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.operation.operation_id, "op-sub");

    let mut conflict = accept_input("op-sub-3", "idem-sub", Some(sub_id), Some(dl_id));
    conflict.request_digest = "different".into();
    assert!(matches!(
        store.accept_download_operation(&conflict),
        Err(StoreError::Plugin(PluginError::IdempotencyConflict))
    ));

    // Record ownership tied to subscribe_id, then delete Subscribe.
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: "crawler-media-owned-op-sub".into(),
            info_hash: Some("0123abcd".into()),
            operation_id: "op-sub".into(),
            subscribe_id: Some(sub_id),
            plugin_id: Some("builtin.boost".into()),
            subject: "subject:1".into(),
            site_id: None,
            enclosure: "https://pt.example/op-sub.torrent".into(),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(4096),
            bytes_digest: Some("sha256-a".into()),
            now_utc: 1_700_000_001_000,
        })
        .unwrap();

    assert!(store.delete_subscribe(sub_id).unwrap());
    drop(store);

    let reopened = Store::open(dir.path()).unwrap();
    assert!(reopened.get_download_operation("op-sub").unwrap().is_some());
    assert!(
        reopened
            .get_download_ownership(dl_id, "crawler-media-owned-op-sub")
            .unwrap()
            .is_some()
    );
}

#[test]
fn download_ownership_refuses_foreign_promotion_and_preserves_broken() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    store
        .accept_download_operation(&accept_input("op-f", "idem-f", None, Some(dl_id)))
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: "foreign-tag".into(),
            info_hash: Some("deadbeef".into()),
            operation_id: "op-f".into(),
            subscribe_id: None,
            plugin_id: Some("builtin.boost".into()),
            subject: "subject:f".into(),
            site_id: None,
            enclosure: "https://pt.example/f.torrent".into(),
            ownership_state: DownloadOwnershipState::Foreign,
            provenance_kind: DownloadProvenanceKind::ExistingForeign,
            verified_bytes: None,
            bytes_digest: None,
            now_utc: 100,
        })
        .unwrap();

    // Promoting foreign row (or same info_hash on same downloader) to Owned must fail.
    let err1 = store.record_download_ownership(&RecordDownloadOwnershipInput {
        downloader_id: dl_id,
        ownership_tag: "foreign-tag".into(),
        info_hash: Some("deadbeef".into()),
        operation_id: "op-f".into(),
        subscribe_id: None,
        plugin_id: Some("builtin.boost".into()),
        subject: "subject:f".into(),
        site_id: None,
        enclosure: "https://pt.example/f.torrent".into(),
        ownership_state: DownloadOwnershipState::Owned,
        provenance_kind: DownloadProvenanceKind::CreatedFresh,
        verified_bytes: Some(100),
        bytes_digest: Some("d1".into()),
        now_utc: 200,
    });
    assert!(matches!(
        err1,
        Err(StoreError::Plugin(
            PluginError::Conflict | PluginError::Denied
        ))
    ));

    // Broken existing task persists distinctly as Broken.
    let broken = store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: "broken-tag".into(),
            info_hash: Some("cafe0001".into()),
            operation_id: "op-f".into(),
            subscribe_id: None,
            plugin_id: None,
            subject: "subject:f".into(),
            site_id: None,
            enclosure: "https://pt.example/f2.torrent".into(),
            ownership_state: DownloadOwnershipState::Broken,
            provenance_kind: DownloadProvenanceKind::BrokenExisting,
            verified_bytes: None,
            bytes_digest: None,
            now_utc: 300,
        })
        .unwrap();
    assert_eq!(broken.ownership_state, DownloadOwnershipState::Broken);
}

#[test]
fn download_ownership_blocks_mixed_case_hash_promotion() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    store
        .accept_download_operation(&accept_input("op-case", "idem-case", None, Some(dl_id)))
        .unwrap();

    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: "foreign-case-tag".into(),
            info_hash: Some("deadbeef".into()),
            operation_id: "op-case".into(),
            subscribe_id: None,
            plugin_id: Some("builtin.boost".into()),
            subject: "subject:case".into(),
            site_id: None,
            enclosure: "https://pt.example/case.torrent".into(),
            ownership_state: DownloadOwnershipState::Foreign,
            provenance_kind: DownloadProvenanceKind::ExistingForeign,
            verified_bytes: None,
            bytes_digest: None,
            now_utc: 100,
        })
        .unwrap();

    let upper_err = store.record_download_ownership(&RecordDownloadOwnershipInput {
        downloader_id: dl_id,
        ownership_tag: "owned-upper-tag".into(),
        info_hash: Some("DEADBEEF".into()),
        operation_id: "op-case".into(),
        subscribe_id: None,
        plugin_id: Some("builtin.boost".into()),
        subject: "subject:case".into(),
        site_id: None,
        enclosure: "https://pt.example/case-owned.torrent".into(),
        ownership_state: DownloadOwnershipState::Owned,
        provenance_kind: DownloadProvenanceKind::CreatedFresh,
        verified_bytes: Some(1234),
        bytes_digest: Some("dig-case".into()),
        now_utc: 150,
    });
    assert!(matches!(
        upper_err,
        Err(StoreError::Plugin(PluginError::Conflict))
    ));

    let found_upper = store
        .find_download_ownership_by_hash(dl_id, "DEADBEEF")
        .unwrap();
    assert_eq!(found_upper.len(), 1);
    assert_eq!(found_upper[0].ownership_tag, "foreign-case-tag");
}

#[test]
fn download_ownership_blocks_two_step_deleted_to_owned_promotion() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    store
        .accept_download_operation(&accept_input("op-two", "idem-two", None, Some(dl_id)))
        .unwrap();

    for (tag, initial_state, initial_prov, hash) in [
        (
            "two-step-foreign-tag",
            DownloadOwnershipState::Foreign,
            DownloadProvenanceKind::ExistingForeign,
            "CAFEBABE",
        ),
        (
            "two-step-broken-tag",
            DownloadOwnershipState::Broken,
            DownloadProvenanceKind::BrokenExisting,
            "FACEB00C",
        ),
    ] {
        let recorded = store
            .record_download_ownership(&RecordDownloadOwnershipInput {
                downloader_id: dl_id,
                ownership_tag: tag.into(),
                info_hash: Some(hash.into()),
                operation_id: "op-two".into(),
                subscribe_id: None,
                plugin_id: Some("builtin.boost".into()),
                subject: "subject:two".into(),
                site_id: None,
                enclosure: "https://pt.example/two-step.torrent".into(),
                ownership_state: initial_state,
                provenance_kind: initial_prov,
                verified_bytes: None,
                bytes_digest: None,
                now_utc: 200,
            })
            .unwrap();
        assert_eq!(
            recorded.info_hash.as_deref(),
            Some(hash.to_ascii_lowercase().as_str())
        );

        let deleted = store
            .record_download_ownership(&RecordDownloadOwnershipInput {
                downloader_id: dl_id,
                ownership_tag: tag.into(),
                info_hash: Some(hash.into()),
                operation_id: "op-two".into(),
                subscribe_id: None,
                plugin_id: Some("builtin.boost".into()),
                subject: "subject:two".into(),
                site_id: None,
                enclosure: "https://pt.example/two-step.torrent".into(),
                ownership_state: DownloadOwnershipState::Deleted,
                provenance_kind: DownloadProvenanceKind::CreatedFresh,
                verified_bytes: None,
                bytes_digest: None,
                now_utc: 250,
            })
            .unwrap();
        assert_eq!(deleted.ownership_state, DownloadOwnershipState::Deleted);
        assert_eq!(deleted.provenance_kind, initial_prov);

        let promo_err = store.record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: tag.into(),
            info_hash: Some(hash.into()),
            operation_id: "op-two".into(),
            subscribe_id: None,
            plugin_id: Some("builtin.boost".into()),
            subject: "subject:two".into(),
            site_id: None,
            enclosure: "https://pt.example/two-step.torrent".into(),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(500),
            bytes_digest: Some("dig-promo".into()),
            now_utc: 300,
        });
        assert!(matches!(
            promo_err,
            Err(StoreError::Plugin(PluginError::Conflict))
        ));
    }
}

#[test]
fn unknown_download_operation_requires_fenced_reconciliation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    let acc = store
        .accept_download_operation(&accept_input("op-u", "idem-u", None, Some(dl_id)))
        .unwrap();
    let submitting = store
        .transition_download_operation(&TransitionDownloadOperationInput {
            operation_id: "op-u".into(),
            expected_revision: acc.operation.revision,
            next_state: DownloadOperationState::Submitting,
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            info_hash: Some("hash-u".into()),
            bytes_digest: Some("dig-u".into()),
            verified_bytes: Some(1024),
            outcome_code: None,
            error_message: None,
            submitted_at: None,
            now_utc: 100,
        })
        .unwrap();
    let unknown = store
        .transition_download_operation(&TransitionDownloadOperationInput {
            operation_id: "op-u".into(),
            expected_revision: submitting.revision,
            next_state: DownloadOperationState::Unknown,
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            info_hash: Some("hash-u".into()),
            bytes_digest: Some("dig-u".into()),
            verified_bytes: Some(1024),
            outcome_code: Some("timeout".into()),
            error_message: Some("post-send timeout".into()),
            submitted_at: None,
            now_utc: 200,
        })
        .unwrap();

    // Ordinary transition cannot leave Unknown.
    let ordinary = store.transition_download_operation(&TransitionDownloadOperationInput {
        operation_id: "op-u".into(),
        expected_revision: unknown.revision,
        next_state: DownloadOperationState::Submitted,
        downloader_id: Some(dl_id),
        endpoint_fingerprint: None,
        info_hash: None,
        bytes_digest: None,
        verified_bytes: None,
        outcome_code: None,
        error_message: None,
        submitted_at: Some(300),
        now_utc: 300,
    });
    assert!(matches!(
        ordinary,
        Err(StoreError::Plugin(PluginError::UnknownEffect))
    ));

    // Stale revision reconciliation fails.
    assert!(
        store
            .reconcile_download_operation(&ReconcileDownloadOperationInput {
                operation_id: "op-u".into(),
                expected_revision: unknown.revision - 1,
                resolved_state: DownloadOperationState::Submitted,
                info_hash: Some("hash-u".into()),
                outcome_code: Some("ok".into()),
                error_message: None,
                ownership: None,
                boost_task_fact: None,
                now_utc: 300,
            })
            .is_err()
    );

    let reconciled = store
        .reconcile_download_operation(&ReconcileDownloadOperationInput {
            operation_id: "op-u".into(),
            expected_revision: unknown.revision,
            resolved_state: DownloadOperationState::Submitted,
            info_hash: Some("hash-u".into()),
            outcome_code: Some("ok".into()),
            error_message: None,
            ownership: Some(RecordDownloadOwnershipInput {
                downloader_id: dl_id,
                ownership_tag: "crawler-media-owned-op-u".into(),
                info_hash: Some("hash-u".into()),
                operation_id: "op-u".into(),
                subscribe_id: None,
                plugin_id: Some("builtin.boost".into()),
                subject: "subject:1".into(),
                site_id: None,
                enclosure: "https://pt.example/op-u.torrent".into(),
                ownership_state: DownloadOwnershipState::Owned,
                provenance_kind: DownloadProvenanceKind::ReconciledOperation,
                verified_bytes: Some(1024),
                bytes_digest: Some("dig-u".into()),
                now_utc: 300,
            }),
            boost_task_fact: None,
            now_utc: 300,
        })
        .unwrap();
    assert_eq!(reconciled.state, DownloadOperationState::Submitted);
    assert!(reconciled.reconciled_at.is_some());
}

pub(crate) fn own_input(
    dl_id: DownloaderId,
    tag: &str,
    hash: Option<&str>,
    op_id: &str,
    sub_id: Option<SubscribeId>,
    plugin_id: Option<&str>,
    state: DownloadOwnershipState,
) -> RecordDownloadOwnershipInput {
    RecordDownloadOwnershipInput {
        downloader_id: dl_id,
        ownership_tag: tag.into(),
        info_hash: hash.map(Into::into),
        operation_id: op_id.into(),
        subscribe_id: sub_id,
        plugin_id: plugin_id.map(Into::into),
        subject: format!("sub:{op_id}"),
        site_id: None,
        enclosure: format!("https://pt.example/{tag}.torrent"),
        ownership_state: state,
        provenance_kind: DownloadProvenanceKind::CreatedFresh,
        verified_bytes: hash.map(|_| 1024),
        bytes_digest: hash.map(|_| format!("dig-{tag}")),
        now_utc: 100,
    }
}

#[test]
fn download_ownership_blocks_cross_plugin_and_subscribe_hijack() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (dl_id, sub_id) = (DownloaderId::new(), SubscribeId::new());
    insert_subscribe(&store, sub_id, None);
    let (owned, del) = (
        DownloadOwnershipState::Owned,
        DownloadOwnershipState::Deleted,
    );

    for (op_id, idem, sub) in [
        ("op-own-alpha", "ik-own-alpha", None),
        ("op-own-beta", "ik-own-beta", None),
        ("op-own-sub", "ik-own-sub", Some(sub_id)),
    ] {
        store
            .accept_download_operation(&accept_input(op_id, idem, sub, Some(dl_id)))
            .unwrap();
    }

    for (tag, hash, op, sub, pid) in [
        (
            "tag-alpha",
            "1111aaaa",
            "op-own-alpha",
            None,
            Some("plugin.alpha"),
        ),
        ("tag-sub", "2222bbbb", "op-own-sub", Some(sub_id), None),
    ] {
        store
            .record_download_ownership(&own_input(dl_id, tag, Some(hash), op, sub, pid, owned))
            .unwrap();
    }

    for (tag, hash, op, sub, pid) in [
        (
            "tag-alpha",
            "1111aaaa",
            "op-own-beta",
            None,
            Some("plugin.beta"),
        ),
        (
            "tag-sub",
            "2222bbbb",
            "op-own-beta",
            None,
            Some("plugin.beta"),
        ),
        ("tag-alpha", "1111aaaa", "op-own-sub", Some(sub_id), None),
    ] {
        let res = store.record_download_ownership(&own_input(
            dl_id,
            tag,
            Some(hash),
            op,
            sub,
            pid,
            owned,
        ));
        assert!(matches!(
            res,
            Err(StoreError::Plugin(PluginError::Conflict))
        ));
    }

    assert_eq!(
        store
            .get_download_ownership(dl_id, "tag-alpha")
            .unwrap()
            .unwrap()
            .plugin_id
            .as_deref(),
        Some("plugin.alpha")
    );
    let sub_row = store
        .get_download_ownership(dl_id, "tag-sub")
        .unwrap()
        .unwrap();
    assert_eq!(
        (sub_row.subscribe_id, sub_row.plugin_id),
        (Some(sub_id), None)
    );

    let deleted = store
        .record_download_ownership(&own_input(
            dl_id,
            "tag-alpha",
            None,
            "op-own-alpha",
            None,
            None,
            del,
        ))
        .unwrap();
    assert_eq!(deleted.ownership_state, del);
    assert_eq!(deleted.plugin_id.as_deref(), Some("plugin.alpha"));
}

#[test]
fn download_ownership_enforces_exact_owner_tuple_and_preserves_owner_on_delete() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (dl_id, sub_a, sub_b) = (DownloaderId::new(), SubscribeId::new(), SubscribeId::new());
    insert_subscribe(&store, sub_a, None);
    insert_subscribe(&store, sub_b, None);
    let (owned, del) = (
        DownloadOwnershipState::Owned,
        DownloadOwnershipState::Deleted,
    );
    let (sa, sb) = (Some(sub_a), Some(sub_b));
    let (pa, pb) = (Some("plugin.alpha"), Some("plugin.beta"));

    for (op, ik, sub) in [
        ("op-sub-a", "ik-sub-a", sa),
        ("op-sub-b", "ik-sub-b", sb),
        ("op-manual", "ik-manual", None),
    ] {
        store
            .accept_download_operation(&accept_input(op, ik, sub, Some(dl_id)))
            .unwrap();
    }
    for (tag, hash, op, sub, pid) in [
        ("tag-sub", "aaaa0001", "op-sub-a", sa, None),
        ("tag-both", "aaaa0002", "op-sub-a", sa, pa),
        ("tag-man", "aaaa0003", "op-manual", None, None),
    ] {
        store
            .record_download_ownership(&own_input(dl_id, tag, Some(hash), op, sub, pid, owned))
            .unwrap();
    }

    for (tag, op, sub, pid, state) in [
        ("tag-sub", "op-sub-b", sb, None, owned),
        ("tag-sub", "op-sub-a", sa, pb, owned),
        ("tag-sub", "op-manual", None, None, owned),
        ("tag-both", "op-sub-b", sb, pa, owned),
        ("tag-both", "op-sub-a", sa, None, owned),
        ("tag-both", "op-manual", None, pa, owned),
        ("tag-both", "op-sub-a", sa, pb, owned),
        ("tag-both", "op-manual", None, None, owned),
        ("tag-both", "op-sub-b", sb, None, del),
        ("tag-man", "op-sub-a", sa, None, owned),
        ("tag-man", "op-manual", None, pa, owned),
        ("tag-man", "op-sub-a", sa, pa, owned),
    ] {
        let res =
            store.record_download_ownership(&own_input(dl_id, tag, None, op, sub, pid, state));
        assert!(
            matches!(res, Err(StoreError::Plugin(PluginError::Conflict))),
            "{tag}: {res:?}"
        );
    }

    let deleted = store
        .record_download_ownership(&own_input(
            dl_id, "tag-both", None, "op-sub-a", None, None, del,
        ))
        .unwrap();
    assert_eq!(
        (
            deleted.ownership_state,
            deleted.subscribe_id,
            deleted.plugin_id.as_deref()
        ),
        (del, sa, pa)
    );
}
