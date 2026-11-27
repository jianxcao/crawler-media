mod outbox_audit;
mod sanitize;

use std::sync::mpsc;
use std::time::Duration;

use store::plugin_events::{
    ClaimDueDeliveriesInput, DeliveryDueCursor, EmitOutboxEventInput, MarkDeadLetterInput,
    MarkDeliveredInput, OutboxDb, OutboxDeliveryStatus, OutboxEventCursor, OutboxEventFilter,
    OutboxFanoutState, ScheduleRetryInput, SeedDeliveryInput, SeedSubscriberDeliveriesInput,
    emit_outbox_event_in_tx, seed_event_fanout_in_tx,
};
use store::plugins::PluginError;
use store::{Db, Store, StoreError};

fn sample_event(event_id: &str, entity_id: &str, created_at: i64) -> EmitOutboxEventInput {
    EmitOutboxEventInput {
        event_id: event_id.into(),
        schema_version: 1,
        event_domain: "download".into(),
        event_kind: "download.after".into(),
        entity_type: "download_operation".into(),
        entity_id: entity_id.into(),
        entity_sequence: None,
        entity_revision: 1,
        cause_id: format!("cause-{event_id}"),
        subject: "system".into(),
        site_id: Some("site-1".into()),
        subscribe_id: Some("sub-1".into()),
        library_id: None,
        handles_json: "[\"h1\"]".into(),
        payload_json: "{\"kind\":\"download_after\",\"data\":{\"operation_id\":\"op-1\"}}".into(),
        occurred_at: created_at,
        created_at,
    }
}

#[test]
fn outbox_db_rejects_cache_and_rolled_back_tx_leaves_zero_rows() {
    assert!(OutboxDb::try_from(Db::Cache).is_err());
    assert_eq!(OutboxDb::try_from(Db::App).unwrap(), OutboxDb::App);
    assert_eq!(
        OutboxDb::try_from(Db::Subscribe).unwrap(),
        OutboxDb::Subscribe
    );
    assert_eq!(OutboxDb::try_from(Db::Library).unwrap(), OutboxDb::Library);

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    for outbox_db in [OutboxDb::App, OutboxDb::Subscribe, OutboxDb::Library] {
        let db: Db = outbox_db.into();
        // Autocommit connection without transaction must be rejected.
        let autocommit_res = store.with_writer(db, |conn| {
            emit_outbox_event_in_tx(conn, &sample_event("ev-auto", "op-1", 1000))
        });
        assert!(matches!(autocommit_res, Err(StoreError::Invalid(_))));

        // Rolled back transaction leaves zero outbox or sequence rows.
        let _ = store.with_writer(db, |conn| {
            let tx = conn.unchecked_transaction()?;
            emit_outbox_event_in_tx(&tx, &sample_event("ev-rb", "op-rb", 1000))?;
            tx.rollback()?;
            Ok(())
        });
        assert!(
            store
                .get_outbox_event(outbox_db, "ev-rb")
                .unwrap()
                .is_none()
        );
    }

    // LibraryConnection::emit_outbox_event rolls back on Err return from library_write.
    let err_res: Result<(), StoreError> = store.library_write(|lib| {
        lib.emit_outbox_event(&sample_event("ev-lib-rb", "media-1", 1000))?;
        Err(StoreError::Invalid("abort transfer".into()))
    });
    assert!(err_res.is_err());
    assert!(
        store
            .get_outbox_event(OutboxDb::Library, "ev-lib-rb")
            .unwrap()
            .is_none()
    );
}

#[test]
fn committed_emit_assigns_monotonic_sequence_surviving_pruning_and_validates_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    for outbox_db in [OutboxDb::App, OutboxDb::Subscribe, OutboxDb::Library] {
        let e1 = store
            .emit_outbox_event(outbox_db, &sample_event("ev-1", "ent-1", 1000))
            .unwrap();
        let e2 = store
            .emit_outbox_event(outbox_db, &sample_event("ev-2", "ent-1", 1010))
            .unwrap();
        let e3 = store
            .emit_outbox_event(outbox_db, &sample_event("ev-3", "ent-1", 1020))
            .unwrap();
        assert_eq!(
            (e1.entity_sequence, e2.entity_sequence, e3.entity_sequence),
            (1, 2, 3)
        );
        assert_eq!(e1.fanout_state, OutboxFanoutState::Pending);

        // Stale explicit sequence rejected.
        let mut stale = sample_event("ev-stale", "ent-1", 1030);
        stale.entity_sequence = Some(2);
        assert!(matches!(
            store.emit_outbox_event(outbox_db, &stale),
            Err(StoreError::Plugin(PluginError::Conflict))
        ));

        // Duplicate event_id rejected.
        assert!(matches!(
            store.emit_outbox_event(outbox_db, &sample_event("ev-1", "ent-2", 1040)),
            Err(StoreError::Plugin(PluginError::Conflict))
        ));

        // Invalid JSON rejected.
        let mut bad_json = sample_event("ev-bad-json", "ent-2", 1050);
        bad_json.payload_json = "not-json".into();
        assert!(matches!(
            store.emit_outbox_event(outbox_db, &bad_json),
            Err(StoreError::Json(_))
        ));

        // Seed 0 subscribers marks fanout_state='seeded' and prune deletes aged rows without resetting sequence.
        store
            .seed_event_fanout(
                outbox_db,
                &SeedSubscriberDeliveriesInput {
                    event_id: "ev-1".into(),
                    plugin_ids: vec![],
                    next_attempt_at: 1000,
                    now_ms: 1000,
                },
            )
            .unwrap();
        let seeded = store.get_outbox_event(outbox_db, "ev-1").unwrap().unwrap();
        assert_eq!(seeded.fanout_state, OutboxFanoutState::Seeded);

        let pruned = store
            .prune_delivered_outbox_events(outbox_db, 1025, 100)
            .unwrap();
        assert!(pruned >= 1);

        let e4 = store
            .emit_outbox_event(outbox_db, &sample_event("ev-4", "ent-1", 2000))
            .unwrap();
        assert_eq!(e4.entity_sequence, 4);
    }
}

#[test]
fn reader_pool_queries_do_not_block_when_writer_lock_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store
        .emit_outbox_event(OutboxDb::Subscribe, &sample_event("ev-r1", "ent-1", 1000))
        .unwrap();

    for db in [Db::App, Db::Subscribe, Db::Library] {
        let outbox_db = OutboxDb::try_from(db).unwrap();
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        std::thread::scope(|s| {
            let store_ref = &store;
            s.spawn(move || {
                store_ref
                    .with_writer(db, |_| {
                        locked_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(())
                    })
                    .unwrap();
            });
            locked_rx.recv().unwrap();

            let start = std::time::Instant::now();
            let _ = store
                .list_outbox_events(outbox_db, &OutboxEventFilter::default(), None, Some(10))
                .unwrap();
            let _ = store
                .list_due_deliveries(outbox_db, None, 2000, None, Some(10))
                .unwrap();
            let _ = store
                .list_dead_letter_deliveries(outbox_db, None, None, Some(10))
                .unwrap();
            let _ = store
                .get_delivery_receipt(outbox_db, "ev-r1", "p1")
                .unwrap();
            let _ = store.get_outbox_diagnostics(outbox_db, 2000).unwrap();
            assert!(
                start.elapsed() < Duration::from_millis(200),
                "ReaderPool read blocked behind held writer lock for {:?}",
                start.elapsed()
            );

            release_tx.send(()).unwrap();
        });
    }
}

#[test]
fn head_of_line_per_entity_ordering_and_dead_letter_unblock() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    for (id, ent, ts) in [
        ("ev-a1", "op-1", 1000),
        ("ev-a2", "op-1", 1010),
        ("ev-a3", "op-1", 1020),
        ("ev-b1", "op-2", 1030),
    ] {
        store
            .emit_outbox_event(OutboxDb::Subscribe, &sample_event(id, ent, ts))
            .unwrap();
        store
            .seed_subscriber_deliveries(
                OutboxDb::Subscribe,
                &[
                    SeedDeliveryInput {
                        event_id: id.into(),
                        plugin_id: "p1".into(),
                        next_attempt_at: 1000,
                    },
                    SeedDeliveryInput {
                        event_id: id.into(),
                        plugin_id: "p2".into(),
                        next_attempt_at: 1000,
                    },
                ],
                1000,
            )
            .unwrap();
    }

    let p1_claim1 = store
        .claim_due_deliveries(
            OutboxDb::Subscribe,
            &ClaimDueDeliveriesInput {
                plugin_id: Some("p1".into()),
                now_ms: 2000,
                lease_duration_ms: 5_000,
                limit: Some(10),
            },
        )
        .unwrap();
    let p1_ids: Vec<&str> = p1_claim1
        .iter()
        .map(|c| c.event.event_id.as_str())
        .collect();
    assert_eq!(p1_ids, vec!["ev-a1", "ev-b1"]);
    assert_eq!(p1_claim1[0].delivery.attempt_count, 1);

    // Plugin p2 progresses independently while p1 is still delivering ev-a1.
    let p2_claim = store
        .claim_due_deliveries(
            OutboxDb::Subscribe,
            &ClaimDueDeliveriesInput {
                plugin_id: Some("p2".into()),
                now_ms: 2000,
                lease_duration_ms: 5_000,
                limit: Some(10),
            },
        )
        .unwrap();
    assert_eq!(p2_claim.len(), 2);

    // Completing ev-a1 unblocks ev-a2 for p1.
    store
        .mark_delivered(
            OutboxDb::Subscribe,
            &MarkDeliveredInput {
                event_id: "ev-a1".into(),
                plugin_id: "p1".into(),
                claim_nonce: p1_claim1[0].claim_nonce.clone(),
                now_ms: 2100,
            },
        )
        .unwrap();

    let p1_claim2 = store
        .claim_due_deliveries(
            OutboxDb::Subscribe,
            &ClaimDueDeliveriesInput {
                plugin_id: Some("p1".into()),
                now_ms: 2200,
                lease_duration_ms: 5_000,
                limit: Some(10),
            },
        )
        .unwrap();
    assert_eq!(p1_claim2.len(), 1);
    assert_eq!(p1_claim2[0].event.event_id, "ev-a2");

    // Dead-lettering ev-a2 unblocks ev-a3 for p1.
    store
        .mark_dead_letter(
            OutboxDb::Subscribe,
            &MarkDeadLetterInput {
                event_id: "ev-a2".into(),
                plugin_id: "p1".into(),
                claim_nonce: p1_claim2[0].claim_nonce.clone(),
                error: "permanent failure".into(),
                now_ms: 2300,
            },
        )
        .unwrap();

    let p1_claim3 = store
        .claim_due_deliveries(
            OutboxDb::Subscribe,
            &ClaimDueDeliveriesInput {
                plugin_id: Some("p1".into()),
                now_ms: 2400,
                lease_duration_ms: 5_000,
                limit: Some(10),
            },
        )
        .unwrap();
    assert_eq!(p1_claim3.len(), 1);
    assert_eq!(p1_claim3[0].event.event_id, "ev-a3");
}

#[test]
fn retry_lease_reclaim_dead_letter_and_receipt_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store
        .emit_outbox_event(OutboxDb::App, &sample_event("ev-s1", "site-1", 1000))
        .unwrap();
    store
        .seed_subscriber_deliveries(
            OutboxDb::App,
            &[SeedDeliveryInput {
                event_id: "ev-s1".into(),
                plugin_id: "p1".into(),
                next_attempt_at: 1000,
            }],
            1000,
        )
        .unwrap();

    // First claim at 2000 with 1000ms lease -> lease_until = 3000, attempt_count = 1.
    let first = store
        .claim_due_deliveries(
            OutboxDb::App,
            &ClaimDueDeliveriesInput {
                plugin_id: Some("p1".into()),
                now_ms: 2000,
                lease_duration_ms: 1000,
                limit: Some(10),
            },
        )
        .unwrap();
    assert_eq!(first[0].delivery.attempt_count, 1);

    // Reclaim after lease expires at 3000 -> new claim_nonce, attempt_count = 2.
    let reclaimed = store
        .claim_due_deliveries(
            OutboxDb::App,
            &ClaimDueDeliveriesInput {
                plugin_id: Some("p1".into()),
                now_ms: 3000,
                lease_duration_ms: 1000,
                limit: Some(10),
            },
        )
        .unwrap();
    assert_eq!(reclaimed[0].delivery.attempt_count, 2);
    assert_ne!(reclaimed[0].claim_nonce, first[0].claim_nonce);

    // Stale claim_nonce fails with Conflict.
    assert!(matches!(
        store.mark_delivered(
            OutboxDb::App,
            &MarkDeliveredInput {
                event_id: "ev-s1".into(),
                plugin_id: "p1".into(),
                claim_nonce: first[0].claim_nonce.clone(),
                now_ms: 3100,
            },
        ),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));

    // Schedule retry does NOT double-increment attempt_count and sanitizes error.
    let retried = store
        .schedule_retry(
            OutboxDb::App,
            &ScheduleRetryInput {
                event_id: "ev-s1".into(),
                plugin_id: "p1".into(),
                claim_nonce: reclaimed[0].claim_nonce.clone(),
                next_attempt_at: 5000,
                error: "https://u:SECRET@host/path?token=XYZ".into(),
                now_ms: 3200,
            },
        )
        .unwrap();
    assert_eq!(retried.status, OutboxDeliveryStatus::Pending);
    assert_eq!(retried.attempt_count, 2);
    assert!(!retried.last_error.as_deref().unwrap().contains("SECRET"));
    assert!(
        store
            .list_due_deliveries(OutboxDb::App, Some("p1"), 4999, None, Some(10))
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn dead_letter_requeue_and_delivered_receipt_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store
        .emit_outbox_event(OutboxDb::App, &sample_event("ev-s1", "site-1", 1000))
        .unwrap();
    store
        .seed_subscriber_deliveries(
            OutboxDb::App,
            &[SeedDeliveryInput {
                event_id: "ev-s1".into(),
                plugin_id: "p1".into(),
                next_attempt_at: 1000,
            }],
            1000,
        )
        .unwrap();

    let claim = store
        .claim_due_deliveries(
            OutboxDb::App,
            &ClaimDueDeliveriesInput {
                plugin_id: Some("p1".into()),
                now_ms: 5000,
                lease_duration_ms: 1000,
                limit: Some(10),
            },
        )
        .unwrap();
    let dl = store
        .mark_dead_letter(
            OutboxDb::App,
            &MarkDeadLetterInput {
                event_id: "ev-s1".into(),
                plugin_id: "p1".into(),
                claim_nonce: claim[0].claim_nonce.clone(),
                error: "   ".into(),
                now_ms: 5100,
            },
        )
        .unwrap();
    assert_eq!(dl.status, OutboxDeliveryStatus::DeadLetter);
    assert_eq!(dl.attempt_count, 1);
    assert!(!dl.last_error.as_deref().unwrap().is_empty());

    let diag = store.get_outbox_diagnostics(OutboxDb::App, 6000).unwrap();
    assert_eq!(diag.dead_letter_count, 1);
    assert_eq!(
        store
            .list_dead_letter_deliveries(OutboxDb::App, None, None, Some(10))
            .unwrap()
            .items
            .len(),
        1
    );

    let requeued = store
        .requeue_dead_letter(OutboxDb::App, "ev-s1", "p1", 6100, 6000)
        .unwrap();
    assert_eq!(requeued.status, OutboxDeliveryStatus::Pending);
    assert_eq!(requeued.attempt_count, 0);

    let final_claim = store
        .claim_due_deliveries(
            OutboxDb::App,
            &ClaimDueDeliveriesInput {
                plugin_id: Some("p1".into()),
                now_ms: 6100,
                lease_duration_ms: 1000,
                limit: Some(10),
            },
        )
        .unwrap();
    store
        .mark_delivered(
            OutboxDb::App,
            &MarkDeliveredInput {
                event_id: "ev-s1".into(),
                plugin_id: "p1".into(),
                claim_nonce: final_claim[0].claim_nonce.clone(),
                now_ms: 6200,
            },
        )
        .unwrap();
    let receipt = store
        .get_delivery_receipt(OutboxDb::App, "ev-s1", "p1")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, OutboxDeliveryStatus::Delivered);
    assert_eq!(receipt.delivered_at, Some(6200));
}

#[test]
fn keyset_pagination_bounds_and_fanout_tx_helper() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    for i in 0..5 {
        let id = format!("ev-{i:02}");
        let ent = format!("ent-{i:02}");
        store
            .with_writer(Db::App, |conn| {
                let tx = conn.unchecked_transaction()?;
                emit_outbox_event_in_tx(&tx, &sample_event(&id, &ent, 1000 + i * 10))?;
                seed_event_fanout_in_tx(
                    &tx,
                    &SeedSubscriberDeliveriesInput {
                        event_id: id.clone(),
                        plugin_ids: vec!["p1".into()],
                        next_attempt_at: 1000 + i * 10,
                        now_ms: 1000 + i * 10,
                    },
                )?;
                tx.commit()?;
                Ok(())
            })
            .unwrap();
    }

    assert!(
        store
            .list_pending_fanout_events(OutboxDb::App, None, Some(10))
            .unwrap()
            .items
            .is_empty()
    );

    let p1 = store
        .list_outbox_events(OutboxDb::App, &OutboxEventFilter::default(), None, Some(2))
        .unwrap();
    assert_eq!(p1.items.len(), 2);
    let c1 = OutboxEventCursor {
        created_at: p1.items[1].created_at,
        event_id: p1.items[1].event_id.clone(),
    };
    let p1_next = store
        .list_outbox_events(
            OutboxDb::App,
            &OutboxEventFilter::default(),
            Some(&c1),
            Some(2),
        )
        .unwrap();
    assert_eq!(p1_next.items[0].event_id, "ev-02");

    let unseeded = store
        .list_unseeded_outbox_events(OutboxDb::App, "p2", None, Some(999))
        .unwrap();
    assert_eq!(unseeded.items.len(), 5);
    assert_eq!(
        store
            .list_delivery_receipts_for_event(OutboxDb::App, "ev-00")
            .unwrap()
            .len(),
        1
    );

    let p2 = store
        .list_due_deliveries(OutboxDb::App, Some("p1"), 2000, None, Some(2))
        .unwrap();
    assert_eq!(p2.items.len(), 2);
    let dc = DeliveryDueCursor {
        next_attempt_at: p2.items[1].delivery.next_attempt_at,
        event_id: p2.items[1].delivery.event_id.clone(),
        plugin_id: p2.items[1].delivery.plugin_id.clone(),
    };
    let p2_next = store
        .list_due_deliveries(OutboxDb::App, Some("p1"), 2000, Some(&dc), Some(2))
        .unwrap();
    assert_eq!(p2_next.items[0].event.event_id, "ev-02");
}

fn claim_p1(
    store: &Store,
    db: OutboxDb,
    now_ms: i64,
    lease_ms: i64,
) -> Vec<store::plugin_events::ClaimedOutboxDelivery> {
    store
        .claim_due_deliveries(
            db,
            &ClaimDueDeliveriesInput {
                plugin_id: Some("p1".into()),
                now_ms,
                lease_duration_ms: lease_ms,
                limit: Some(10),
            },
        )
        .unwrap()
}

#[test]
fn requeue_dead_letter_preserves_single_in_flight_per_entity() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    for (id, ts) in [("ev-1", 1000), ("ev-2", 1010), ("ev-3", 1020)] {
        store
            .emit_outbox_event(OutboxDb::Subscribe, &sample_event(id, "ent-1", ts))
            .unwrap();
        let seed = SeedDeliveryInput {
            event_id: id.into(),
            plugin_id: "p1".into(),
            next_attempt_at: 1000,
        };
        store
            .seed_subscriber_deliveries(OutboxDb::Subscribe, &[seed], 1000)
            .unwrap();
    }

    let c1 = claim_p1(&store, OutboxDb::Subscribe, 2000, 5000);
    assert_eq!(c1[0].event.event_id, "ev-1");
    let dl = MarkDeadLetterInput {
        event_id: "ev-1".into(),
        plugin_id: "p1".into(),
        claim_nonce: c1[0].claim_nonce.clone(),
        error: "perm".into(),
        now_ms: 2100,
    };
    store.mark_dead_letter(OutboxDb::Subscribe, &dl).unwrap();

    // Claim ev-2 (seq=2) into active 'delivering' state, then requeue ev-1 (seq=1).
    let c2 = claim_p1(&store, OutboxDb::Subscribe, 2200, 6000);
    assert_eq!(c2[0].event.event_id, "ev-2");
    let requeued = store
        .requeue_dead_letter(OutboxDb::Subscribe, "ev-1", "p1", 2300, 2300)
        .unwrap();
    assert_eq!(requeued.status, OutboxDeliveryStatus::Pending);

    // Neither list_due_deliveries nor claim_due_deliveries may surface/claim ev-1 while ev-2 is delivering.
    let due = store
        .list_due_deliveries(OutboxDb::Subscribe, Some("p1"), 2400, None, Some(10))
        .unwrap();
    assert!(
        due.items.is_empty(),
        "due deliveries while seq=2 delivering: {:?}",
        due.items
    );
    let concurrent = claim_p1(&store, OutboxDb::Subscribe, 2400, 5000);
    assert!(
        concurrent.is_empty(),
        "concurrent delivering on same entity: {:?}",
        concurrent
    );

    // If ev-2's lease expires while ev-1 is pending, ev-2 reclaims cleanly (no deadlock, still single-in-flight).
    let reclaimed = claim_p1(&store, OutboxDb::Subscribe, 9000, 5000);
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].event.event_id, "ev-2");
    assert_eq!(reclaimed[0].delivery.attempt_count, 2);

    // Completing ev-2 unblocks ev-1.
    let done2 = MarkDeliveredInput {
        event_id: "ev-2".into(),
        plugin_id: "p1".into(),
        claim_nonce: reclaimed[0].claim_nonce.clone(),
        now_ms: 9100,
    };
    store.mark_delivered(OutboxDb::Subscribe, &done2).unwrap();
    let after = claim_p1(&store, OutboxDb::Subscribe, 9200, 5000);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].event.event_id, "ev-1");
}
