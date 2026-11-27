use rusqlite::Connection;
use sha2::{Digest, Sha256};
use store::{Store, StoreError};

fn db_hash(dir: &std::path::Path, name: &str) -> Vec<u8> {
    Sha256::digest(std::fs::read(dir.join(name)).unwrap()).to_vec()
}

fn seed_outbox_row(conn: &Connection, event_id: &str, seq: i64) {
    conn.execute(
        "INSERT INTO plugin_event_outbox(
            event_id, schema_version, event_domain, event_kind,
            entity_type, entity_id, entity_sequence, entity_revision,
            cause_id, subject, handles_json, payload_json,
            occurred_at, fanout_state, created_at
         ) VALUES (?1, 1, 'download', 'download.after', 'download_operation', 'op-1', ?2, 1, 'cause-1', 'system', '[]', '{}', 1000, 'pending', 1000)",
        rusqlite::params![event_id, seq],
    )
    .unwrap();
}

#[test]
fn outbox_schema_parity_rejects_missing_or_mutated_objects_and_preserves_versions() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());

    for (db_file, expected_ver) in [
        ("app.db", store::schema::layout::V1_USER_VERSION),
        (
            "subscribe.db",
            store::schema::layout::SUBSCRIBE_USER_VERSION,
        ),
        ("library.db", store::schema::layout::LIBRARY_USER_VERSION),
    ] {
        let conn = Connection::open(dir.path().join(db_file)).unwrap();
        let ver: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ver, expected_ver, "{db_file} user_version mismatch");
    }

    let drop_targets = [
        "DROP TABLE plugin_event_entity_sequences",
        "DROP TABLE plugin_event_deliveries",
        "DROP TABLE plugin_event_outbox",
        "DROP INDEX plugin_event_outbox_pending_fanout_idx",
        "DROP INDEX plugin_event_outbox_created_idx",
        "DROP INDEX plugin_event_outbox_domain_created_idx",
        "DROP INDEX plugin_event_outbox_entity_created_idx",
        "DROP INDEX plugin_event_deliveries_due_idx",
        "DROP INDEX plugin_event_deliveries_lease_idx",
        "DROP INDEX plugin_event_deliveries_entity_order_idx",
        "DROP INDEX plugin_event_deliveries_dead_letter_idx",
        "DROP INDEX plugin_event_deliveries_plugin_status_idx",
    ];

    for (db_file, is_app) in [
        ("app.db", true),
        ("subscribe.db", false),
        ("library.db", false),
    ] {
        for bad_sql in drop_targets {
            let case_dir = tempfile::tempdir().unwrap();
            drop(Store::open(case_dir.path()).unwrap());
            let conn = Connection::open(case_dir.path().join(db_file)).unwrap();
            conn.execute_batch(bad_sql).unwrap();
            drop(conn);
            let before = db_hash(case_dir.path(), db_file);
            let res = Store::open(case_dir.path());
            if is_app {
                assert!(
                    matches!(res, Err(StoreError::Invalid(_))),
                    "{db_file} {bad_sql}: expected Invalid"
                );
            } else {
                assert!(
                    matches!(res, Err(StoreError::LayoutMismatch(_))),
                    "{db_file} {bad_sql}: expected LayoutMismatch"
                );
            }
            assert_eq!(db_hash(case_dir.path(), db_file), before);
        }
    }
}

#[test]
fn outbox_ddl_enforces_check_constraints_unique_keys_and_fk_cascade() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());

    for db_file in ["app.db", "subscribe.db", "library.db"] {
        let conn = Connection::open(dir.path().join(db_file)).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        seed_outbox_row(&conn, "ev-1", 1);

        // Duplicate (entity_type, entity_id, entity_sequence) rejected.
        assert!(
            conn.execute(
                "INSERT INTO plugin_event_outbox(
                    event_id, event_domain, event_kind, entity_type, entity_id,
                    entity_sequence, cause_id, payload_json, occurred_at, created_at
                 ) VALUES ('ev-dup', 'download', 'download.after', 'download_operation', 'op-1', 1, 'cause-1', '{}', 1000, 1000)",
                [],
            )
            .is_err()
        );

        // Non-object payload_json, non-array handles_json, and bad fanout_state rejected.
        for bad_payload in ["not-json", "[]", "123"] {
            assert!(
                conn.execute(
                    "INSERT INTO plugin_event_outbox(
                        event_id, event_domain, event_kind, entity_type, entity_id,
                        entity_sequence, cause_id, payload_json, occurred_at, created_at
                     ) VALUES ('ev-bad-p', 'd', 'k', 't', 'i', 1, 'c', ?1, 1000, 1000)",
                    [bad_payload],
                )
                .is_err()
            );
        }
        assert!(
            conn.execute(
                "INSERT INTO plugin_event_outbox(
                    event_id, event_domain, event_kind, entity_type, entity_id,
                    entity_sequence, cause_id, handles_json, payload_json, occurred_at, created_at
                 ) VALUES ('ev-bad-h', 'd', 'k', 't', 'i', 1, 'c', '{}', '{}', 1000, 1000)",
                [],
            )
            .is_err()
        );

        // Delivering without lease/nonce/attempt_count > 0 rejected.
        assert!(
            conn.execute(
                "INSERT INTO plugin_event_deliveries(
                    event_id, plugin_id, entity_type, entity_id, entity_sequence,
                    status, attempt_count, next_attempt_at, created_at, updated_at
                 ) VALUES ('ev-1', 'p1', 'download_operation', 'op-1', 1, 'delivering', 0, 1000, 1000, 1000)",
                [],
            )
            .is_err()
        );
        // Dead-letter without non-empty last_error rejected.
        assert!(
            conn.execute(
                "INSERT INTO plugin_event_deliveries(
                    event_id, plugin_id, entity_type, entity_id, entity_sequence,
                    status, attempt_count, next_attempt_at, dead_lettered_at, created_at, updated_at
                 ) VALUES ('ev-1', 'p1', 'download_operation', 'op-1', 1, 'dead_letter', 1, 1000, 1000, 1000, 1000)",
                [],
            )
            .is_err()
        );
        // Orphan delivery FK rejected.
        assert!(
            conn.execute(
                "INSERT INTO plugin_event_deliveries(
                    event_id, plugin_id, entity_type, entity_id, entity_sequence,
                    status, attempt_count, next_attempt_at, created_at, updated_at
                 ) VALUES ('missing-ev', 'p1', 'download_operation', 'op-1', 1, 'pending', 0, 1000, 1000, 1000)",
                [],
            )
            .is_err()
        );

        // Cascade delete removes deliveries.
        conn.execute(
            "INSERT INTO plugin_event_deliveries(
                event_id, plugin_id, entity_type, entity_id, entity_sequence,
                status, attempt_count, next_attempt_at, created_at, updated_at
             ) VALUES ('ev-1', 'p1', 'download_operation', 'op-1', 1, 'pending', 0, 1000, 1000, 1000)",
            [],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM plugin_event_outbox WHERE event_id = 'ev-1'",
            [],
        )
        .unwrap();
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM plugin_event_deliveries WHERE event_id = 'ev-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
    }
}

#[test]
fn outbox_query_plans_use_named_indexes_without_table_scans() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let conn = Connection::open(dir.path().join("app.db")).unwrap();

    let queries = [
        "EXPLAIN QUERY PLAN SELECT event_id FROM plugin_event_outbox WHERE fanout_state = 'pending' ORDER BY created_at ASC, event_id ASC LIMIT 10",
        "EXPLAIN QUERY PLAN SELECT event_id, plugin_id FROM plugin_event_deliveries WHERE status = 'pending' AND next_attempt_at <= 1000 ORDER BY next_attempt_at ASC, event_id ASC, plugin_id ASC LIMIT 10",
        "EXPLAIN QUERY PLAN SELECT event_id, plugin_id FROM plugin_event_deliveries WHERE status = 'delivering' AND lease_until <= 1000 ORDER BY lease_until ASC, event_id ASC, plugin_id ASC LIMIT 10",
        "EXPLAIN QUERY PLAN SELECT event_id, plugin_id FROM plugin_event_deliveries WHERE status = 'dead_letter' ORDER BY dead_lettered_at DESC, event_id ASC, plugin_id ASC LIMIT 10",
    ];
    for sql in queries {
        let mut stmt = conn.prepare(sql).unwrap();
        let details: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let joined = details.join(" | ");
        assert!(
            joined.contains("INDEX plugin_event_") && !joined.contains("TEMP B-TREE"),
            "expected named index without temp b-tree in `{sql}`: {joined}"
        );
    }
}

#[test]
fn outbox_listing_and_pruning_query_plans_avoid_scans_and_temp_btrees() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let conn = Connection::open(dir.path().join("app.db")).unwrap();

    let cols = "event_id, schema_version, event_domain, event_kind, entity_type, entity_id, entity_sequence, entity_revision, cause_id, subject, site_id, subscribe_id, library_id, handles_json, payload_json, occurred_at, fanout_state, created_at";
    let queries = [
        format!("EXPLAIN QUERY PLAN SELECT {cols} FROM plugin_event_outbox ORDER BY created_at ASC, event_id ASC LIMIT 10"),
        format!("EXPLAIN QUERY PLAN SELECT {cols} FROM plugin_event_outbox WHERE (created_at, event_id) > (1000, 'e1') ORDER BY created_at ASC, event_id ASC LIMIT 10"),
        format!("EXPLAIN QUERY PLAN SELECT {cols} FROM plugin_event_outbox WHERE event_domain = 'd' AND event_kind = 'k' ORDER BY created_at ASC, event_id ASC LIMIT 10"),
        format!("EXPLAIN QUERY PLAN SELECT {cols} FROM plugin_event_outbox WHERE event_domain = 'd' AND event_kind = 'k' AND (created_at, event_id) > (1000, 'e1') ORDER BY created_at ASC, event_id ASC LIMIT 10"),
        format!("EXPLAIN QUERY PLAN SELECT {cols} FROM plugin_event_outbox WHERE entity_type = 't' AND entity_id = 'i' ORDER BY created_at ASC, event_id ASC LIMIT 10"),
        "EXPLAIN QUERY PLAN SELECT o.event_id, o.created_at, o.payload_json FROM plugin_event_outbox o WHERE NOT EXISTS (SELECT 1 FROM plugin_event_deliveries d WHERE d.event_id = o.event_id AND d.plugin_id = 'p1') ORDER BY o.created_at ASC, o.event_id ASC LIMIT 10".to_string(),
        "EXPLAIN QUERY PLAN SELECT o.event_id, o.created_at, o.payload_json FROM plugin_event_outbox o WHERE NOT EXISTS (SELECT 1 FROM plugin_event_deliveries d WHERE d.event_id = o.event_id AND d.plugin_id = 'p1') AND (o.created_at, o.event_id) > (1000, 'e1') ORDER BY o.created_at ASC, o.event_id ASC LIMIT 10".to_string(),
        "EXPLAIN QUERY PLAN SELECT event_id FROM plugin_event_outbox WHERE fanout_state = 'seeded' AND created_at < 1000 AND NOT EXISTS (SELECT 1 FROM plugin_event_deliveries d WHERE d.event_id = plugin_event_outbox.event_id AND d.status IN ('pending', 'delivering', 'dead_letter')) ORDER BY created_at ASC, event_id ASC LIMIT 10".to_string(),
    ];
    for sql in queries {
        let mut stmt = conn.prepare(&sql).unwrap();
        let details: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let has_bare_table_scan = details.iter().any(|d| {
            (d == "SCAN plugin_event_outbox" || d == "SCAN o")
                || ((d.starts_with("SCAN plugin_event_outbox ") || d.starts_with("SCAN o "))
                    && !d.contains("INDEX "))
        });
        let joined = details.join(" | ");
        assert!(
            !has_bare_table_scan
                && !joined.contains("TEMP B-TREE")
                && joined.contains("INDEX plugin_event_outbox_"),
            "unexpected table scan or temp b-tree in `{sql}`: {joined}"
        );
    }
}

#[test]
fn outbox_list_filters_and_cursors_return_expected_rows() {
    use store::plugin_events::{
        EmitOutboxEventInput, OutboxDb, OutboxEventCursor, OutboxEventFilter,
    };

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    for (id, domain, kind, etype, eid, ts) in [
        (
            "e1",
            "download",
            "download.after",
            "download_op",
            "op-1",
            1000,
        ),
        (
            "e2",
            "download",
            "download.after",
            "download_op",
            "op-1",
            1010,
        ),
        ("e3", "site", "site.checkin.after", "site", "s-1", 1020),
    ] {
        store
            .emit_outbox_event(
                OutboxDb::App,
                &EmitOutboxEventInput {
                    event_id: id.into(),
                    schema_version: 1,
                    event_domain: domain.into(),
                    event_kind: kind.into(),
                    entity_type: etype.into(),
                    entity_id: eid.into(),
                    entity_sequence: None,
                    entity_revision: 1,
                    cause_id: format!("c-{id}"),
                    subject: "system".into(),
                    site_id: None,
                    subscribe_id: None,
                    library_id: None,
                    handles_json: "[]".into(),
                    payload_json: "{}".into(),
                    occurred_at: ts,
                    created_at: ts,
                },
            )
            .unwrap();
    }

    let by_domain = store
        .list_outbox_events(
            OutboxDb::App,
            &OutboxEventFilter {
                event_domain: Some("download".into()),
                event_kind: Some("download.after".into()),
                ..Default::default()
            },
            None,
            Some(1),
        )
        .unwrap();
    assert_eq!(by_domain.items.len(), 1);
    assert_eq!(by_domain.items[0].event_id, "e1");
    assert!(by_domain.next_cursor.is_some());

    let next_page = store
        .list_outbox_events(
            OutboxDb::App,
            &OutboxEventFilter {
                event_domain: Some("download".into()),
                event_kind: Some("download.after".into()),
                ..Default::default()
            },
            Some(&OutboxEventCursor {
                created_at: by_domain.items[0].created_at,
                event_id: by_domain.items[0].event_id.clone(),
            }),
            Some(10),
        )
        .unwrap();
    assert_eq!(next_page.items.len(), 1);
    assert_eq!(next_page.items[0].event_id, "e2");

    let by_entity = store
        .list_outbox_events(
            OutboxDb::App,
            &OutboxEventFilter {
                entity_type: Some("site".into()),
                entity_id: Some("s-1".into()),
                ..Default::default()
            },
            None,
            Some(10),
        )
        .unwrap();
    assert_eq!(by_entity.items.len(), 1);
    assert_eq!(by_entity.items[0].event_id, "e3");
}

#[test]
fn prune_delivered_outbox_events_preserves_active_and_dead_letter_rows() {
    use store::plugin_events::{
        ClaimDueDeliveriesInput, DeadLetterCursor, EmitOutboxEventInput, MarkDeadLetterInput,
        MarkDeliveredInput, OutboxDb, SeedSubscriberDeliveriesInput,
    };

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    for i in 0..3 {
        let id = format!("ev-{i:02}");
        let ts = 1000 + i * 10;
        store
            .emit_outbox_event(
                OutboxDb::App,
                &EmitOutboxEventInput {
                    event_id: id.clone(),
                    schema_version: 1,
                    event_domain: "download".into(),
                    event_kind: "download.after".into(),
                    entity_type: "download_operation".into(),
                    entity_id: format!("ent-{i:02}"),
                    entity_sequence: None,
                    entity_revision: 1,
                    cause_id: format!("cause-{id}"),
                    subject: "system".into(),
                    site_id: None,
                    subscribe_id: None,
                    library_id: None,
                    handles_json: "[]".into(),
                    payload_json: "{}".into(),
                    occurred_at: ts,
                    created_at: ts,
                },
            )
            .unwrap();
        store
            .seed_event_fanout(
                OutboxDb::App,
                &SeedSubscriberDeliveriesInput {
                    event_id: id,
                    plugin_ids: vec!["p1".into()],
                    next_attempt_at: ts,
                    now_ms: ts,
                },
            )
            .unwrap();
    }

    let claimed = store
        .claim_due_deliveries(
            OutboxDb::App,
            &ClaimDueDeliveriesInput {
                plugin_id: Some("p1".into()),
                now_ms: 2000,
                lease_duration_ms: 5000,
                limit: Some(3),
            },
        )
        .unwrap();
    assert_eq!(claimed.len(), 3);
    store
        .mark_delivered(
            OutboxDb::App,
            &MarkDeliveredInput {
                event_id: claimed[0].event.event_id.clone(),
                plugin_id: "p1".into(),
                claim_nonce: claimed[0].claim_nonce.clone(),
                now_ms: 2100,
            },
        )
        .unwrap();
    store
        .mark_dead_letter(
            OutboxDb::App,
            &MarkDeadLetterInput {
                event_id: claimed[1].event.event_id.clone(),
                plugin_id: "p1".into(),
                claim_nonce: claimed[1].claim_nonce.clone(),
                error: "perm".into(),
                now_ms: 2100,
            },
        )
        .unwrap();
    let dl_page = store
        .list_dead_letter_deliveries(
            OutboxDb::App,
            Some("p1"),
            Some(&DeadLetterCursor {
                dead_lettered_at: 9999,
                event_id: String::new(),
                plugin_id: String::new(),
            }),
            Some(10),
        )
        .unwrap();
    assert_eq!(dl_page.items.len(), 1);

    let pruned = store
        .prune_delivered_outbox_events(OutboxDb::App, 1035, 100)
        .unwrap();
    assert_eq!(pruned, 1);
    assert!(
        store
            .get_outbox_event(OutboxDb::App, "ev-00")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_outbox_event(OutboxDb::App, "ev-01")
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_outbox_event(OutboxDb::App, "ev-02")
            .unwrap()
            .is_some()
    );
}
