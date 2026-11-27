use std::sync::{Arc, Barrier, mpsc};
use std::time::{Duration, Instant};

use domain::{DownloaderId, LibraryId, SubscribeId};
use store::download_operations::*;
use store::plugins::PluginError;
use store::{Db, Store, StoreError};

use super::{accept_input, insert_subscribe};

#[test]
fn concurrent_boost_final_reservations_serialize_quota_cas() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let dl_id = DownloaderId::new();
    let budget = 10_i64 * 1024 * 1024 * 1024;
    let six_gib = 6_i64 * 1024 * 1024 * 1024;

    let mut revs = Vec::new();
    for i in 1..=2 {
        let op_id = format!("op-b{i}");
        store
            .accept_download_operation(&accept_input(&op_id, &format!("ik-{i}"), None, Some(dl_id)))
            .unwrap();
        let res = store
            .reserve_boost_provisional(&ReserveBoostProvisionalInput {
                reservation_id: format!("res-{i}"),
                operation_id: op_id,
                quota_key: "pool:default".into(),
                plugin_id: "builtin.boost".into(),
                site_id: None,
                downloader_id: dl_id,
                info_hash: Some(format!("hash-{i}")),
                estimated_bytes: six_gib,
                expires_at: None,
                now_utc: 100,
            })
            .unwrap();
        revs.push(res.revision);
    }

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for (idx, rev) in revs.into_iter().enumerate() {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let i = idx + 1;
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            store.commit_boost_final_reservation(&CommitBoostFinalInput {
                operation_id: format!("op-b{i}"),
                expected_reservation_revision: rev,
                info_hash: format!("hash-{i}"),
                bytes_digest: format!("dig-{i}"),
                verified_bytes: six_gib,
                budget_bytes: budget,
                max_add_per_hour: None,
                mark_operation_submitting: true,
                now_utc: 200,
            })
        }));
    }
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let ok_count = results.iter().filter(|r| r.is_ok()).count();
    let quota_err_count = results
        .iter()
        .filter(|r| matches!(r, Err(StoreError::Plugin(PluginError::QuotaExceeded))))
        .count();
    assert_eq!(ok_count, 1);
    assert_eq!(quota_err_count, 1);
}

fn seed_unknown_held_boost_task(store: &Store, dl_id: DownloaderId) -> BoostReservationRecord {
    store
        .accept_download_operation(&accept_input("op-r1", "ik-r1", None, Some(dl_id)))
        .unwrap();
    let r1 = store
        .reserve_boost_provisional(&ReserveBoostProvisionalInput {
            reservation_id: "res-r1".into(),
            operation_id: "op-r1".into(),
            quota_key: "pool:main".into(),
            plugin_id: "builtin.boost".into(),
            site_id: None,
            downloader_id: dl_id,
            info_hash: Some("hash-r1".into()),
            estimated_bytes: 1_000,
            expires_at: None,
            now_utc: 100,
        })
        .unwrap();
    let r1 = store
        .commit_boost_final_reservation(&CommitBoostFinalInput {
            operation_id: "op-r1".into(),
            expected_reservation_revision: r1.revision,
            info_hash: "hash-r1".into(),
            bytes_digest: "dig-r1".into(),
            verified_bytes: 2_000,
            budget_bytes: 10_000,
            max_add_per_hour: Some(5),
            mark_operation_submitting: true,
            now_utc: 200,
        })
        .unwrap();
    store
        .mark_boost_reservation_unknown_held("op-r1", r1.revision, 300)
        .unwrap()
}

#[test]
fn unknown_held_and_retained_files_occupy_quota_without_double_counting() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    let r1 = seed_unknown_held_boost_task(&store, dl_id);
    drop(store);

    let store = Store::open(dir.path()).unwrap();
    let snap = store
        .get_boost_snapshot_facts("pool:main", &[dl_id])
        .unwrap();
    assert_eq!(snap.reserved_bytes, 2_000);
    assert_eq!(snap.used_bytes, 0);
    assert_eq!(snap.retained_file_bytes, 0);

    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: "crawler-media-owned-op-r1".into(),
            info_hash: Some("hash-r1".into()),
            operation_id: "op-r1".into(),
            subscribe_id: None,
            plugin_id: Some("builtin.boost".into()),
            subject: "subject:1".into(),
            site_id: None,
            enclosure: "https://pt.example/op-r1.torrent".into(),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::ReconciledOperation,
            verified_bytes: Some(2_000),
            bytes_digest: Some("dig-r1".into()),
            now_utc: 400,
        })
        .unwrap();
    let (_res, tf) = store
        .finalize_boost_reservation_to_task(&FinalizeBoostTaskInput {
            operation_id: "op-r1".into(),
            expected_reservation_revision: r1.revision,
            task_fact: UpsertBoostTaskFactInput {
                task_fact_id: "tf-1".into(),
                plugin_id: "builtin.boost".into(),
                quota_key: "pool:main".into(),
                downloader_id: dl_id,
                ownership_tag: "crawler-media-owned-op-r1".into(),
                info_hash: "hash-r1".into(),
                site_id: None,
                state: BoostTaskState::Seeding,
                occupied_bytes: 2_000,
                retained_bytes: 0,
                hr_active: None,
                hr_until: None,
                hold_until: Some(500),
                free_until: None,
                protection_state: BoostProtectionState::Unknown,
                protection_reason: None,
                observed_at: 400,
                expected_revision: None,
                now_utc: 400,
            },
        })
        .unwrap();
    let snap2 = store
        .get_boost_snapshot_facts("pool:main", &[dl_id])
        .unwrap();
    assert_eq!(snap2.reserved_bytes, 0);
    assert_eq!(snap2.used_bytes, 2_000);

    let tf = store
        .mark_boost_task_retained("tf-1", tf.revision, 2_000, 450)
        .unwrap();
    assert_eq!(tf.state, BoostTaskState::RetainedOnly);
    assert_eq!(
        store
            .get_download_ownership(dl_id, "crawler-media-owned-op-r1")
            .unwrap()
            .unwrap()
            .ownership_state,
        DownloadOwnershipState::Deleted
    );
    assert_cleanup_plan_hr_and_ttl_enforced(&store, dl_id, &tf);
}

fn assert_cleanup_plan_hr_and_ttl_enforced(
    store: &Store,
    dl_id: DownloaderId,
    tf: &BoostTaskFactRecord,
) {
    // Fail-closed HR/hold protection rejects cleanup plan when hr_active == None or hold active.
    assert!(
        store
            .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
                task_fact_id: "tf-1".into(),
                expected_revision: tf.revision,
                cleanup_plan_handle: "plan-1".into(),
                cleanup_plan_json: "{\"paths\":[\"/a\"]}".into(),
                ttl_secs: 600,
                now_utc: 460,
            })
            .is_err()
    );

    // Update task to explicit hr_active=Some(false), hold expired, eligible -> plan succeeds.
    let tf = store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: "tf-1".into(),
            plugin_id: "builtin.boost".into(),
            quota_key: "pool:main".into(),
            downloader_id: dl_id,
            ownership_tag: "crawler-media-owned-op-r1".into(),
            info_hash: "hash-r1".into(),
            site_id: None,
            state: BoostTaskState::RetainedOnly,
            occupied_bytes: 0,
            retained_bytes: 2_000,
            hr_active: Some(false),
            hr_until: None,
            hold_until: Some(500),
            free_until: None,
            protection_state: BoostProtectionState::Eligible,
            protection_reason: None,
            observed_at: 600,
            expected_revision: Some(tf.revision),
            now_utc: 600,
        })
        .unwrap();
    let planned = store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: "tf-1".into(),
            expected_revision: tf.revision,
            cleanup_plan_handle: "plan-1".into(),
            cleanup_plan_json: "{\"paths\":[\"/a\"]}".into(),
            ttl_secs: 600,
            now_utc: 600,
        })
        .unwrap();
    // Expired completion fails; timely completion zeroes retained_file_bytes.
    assert!(
        store
            .complete_boost_cleanup("plan-1", planned.revision, 600 + 601)
            .is_err()
    );
    let cleaned = store
        .complete_boost_cleanup("plan-1", planned.revision, 700)
        .unwrap();
    assert_eq!(cleaned.state, BoostTaskState::Cleaned);
    assert_eq!(cleaned.retained_bytes, 0);
}

#[test]
fn subscribe_defer_enforces_10_attempts_and_72h_cap_then_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let sub_id = SubscribeId::new();
    insert_subscribe(&store, sub_id, None);
    let binding = store.subscribe_policy_binding(sub_id).unwrap().unwrap();

    for attempt in 1..=10 {
        let rec = store
            .record_subscribe_defer(&RecordSubscribeDeferInput {
                defer_id: format!("def-{attempt}"),
                subscribe_id: sub_id,
                subscribe_generation: binding.jobs_sync_generation.clone(),
                policy_revision: binding.policy_revision,
                enclosure: "https://pt.example/a.torrent".into(),
                info_hash: Some("hash-a".into()),
                plugin_id: Some("p1".into()),
                hook_id: "download.before".into(),
                reason: "wait".into(),
                next_retry_at: attempt * 100,
                now_utc: attempt * 100,
            })
            .unwrap();
        assert_eq!(rec.attempt_count, attempt);
        assert_eq!(rec.state, SubscribeDeferState::Deferred);
    }

    // Attempt 11 transitions to Rejected.
    let eleventh = store
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: "def-11".into(),
            subscribe_id: sub_id,
            subscribe_generation: binding.jobs_sync_generation.clone(),
            policy_revision: binding.policy_revision,
            enclosure: "https://pt.example/a.torrent".into(),
            info_hash: Some("hash-a".into()),
            plugin_id: Some("p1".into()),
            hook_id: "download.before".into(),
            reason: "wait".into(),
            next_retry_at: 2_000,
            now_utc: 2_000,
        })
        .unwrap();
    assert_eq!(eleventh.state, SubscribeDeferState::Rejected);

    // Fresh candidate exceeding 72h cap (259_200_000 ms or 259_200 secs — check seconds vs ms!).
    let r1 = store
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: "def-72h-1".into(),
            subscribe_id: sub_id,
            subscribe_generation: binding.jobs_sync_generation.clone(),
            policy_revision: binding.policy_revision,
            enclosure: "https://pt.example/b.torrent".into(),
            info_hash: None,
            plugin_id: Some("p1".into()),
            hook_id: "download.before".into(),
            reason: "wait".into(),
            next_retry_at: 10_000,
            now_utc: 1_000,
        })
        .unwrap();
    let expired = store
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: "def-72h-2".into(),
            subscribe_id: sub_id,
            subscribe_generation: binding.jobs_sync_generation,
            policy_revision: binding.policy_revision,
            enclosure: "https://pt.example/b.torrent".into(),
            info_hash: None,
            plugin_id: Some("p1".into()),
            hook_id: "download.before".into(),
            reason: "wait".into(),
            next_retry_at: r1.max_deadline_at + 10,
            now_utc: r1.max_deadline_at,
        })
        .unwrap();
    assert_eq!(expired.state, SubscribeDeferState::Rejected);
    assert!(
        store
            .clear_rejected_subscribe_defer(sub_id, "https://pt.example/b.torrent", 999_999)
            .unwrap()
    );
}

#[test]
fn subscribe_update_detach_and_recreate_invalidate_prior_defers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lib_id = LibraryId::new();
    let sub_id = SubscribeId::new();
    let mut sub = insert_subscribe(&store, sub_id, Some(lib_id));
    let b1 = store.subscribe_policy_binding(sub_id).unwrap().unwrap();

    store
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: "def-u1".into(),
            subscribe_id: sub_id,
            subscribe_generation: b1.jobs_sync_generation.clone(),
            policy_revision: b1.policy_revision,
            enclosure: "https://pt.example/u1.torrent".into(),
            info_hash: None,
            plugin_id: Some("p1".into()),
            hook_id: "download.before".into(),
            reason: "wait".into(),
            next_retry_at: 50,
            now_utc: 10,
        })
        .unwrap();
    sub.wash_cut = true;
    store.update_subscribe(&sub).unwrap();
    let b2 = store.subscribe_policy_binding(sub_id).unwrap().unwrap();
    assert!(b2.policy_revision > b1.policy_revision);
    assert_eq!(
        store
            .get_subscribe_defer(sub_id, "https://pt.example/u1.torrent")
            .unwrap()
            .unwrap()
            .state,
        SubscribeDeferState::Invalidated
    );

    // Detaching library also increments policy_revision and invalidates active defers.
    store
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: "def-u2".into(),
            subscribe_id: sub_id,
            subscribe_generation: b2.jobs_sync_generation.clone(),
            policy_revision: b2.policy_revision,
            enclosure: "https://pt.example/u2.torrent".into(),
            info_hash: None,
            plugin_id: Some("p1".into()),
            hook_id: "download.before".into(),
            reason: "wait".into(),
            next_retry_at: 50,
            now_utc: 20,
        })
        .unwrap();
    store.detach_subscribes_from_library(lib_id).unwrap();
    let b3 = store.subscribe_policy_binding(sub_id).unwrap().unwrap();
    assert!(b3.policy_revision > b2.policy_revision);
    assert_eq!(
        store
            .get_subscribe_defer(sub_id, "https://pt.example/u2.torrent")
            .unwrap()
            .unwrap()
            .state,
        SubscribeDeferState::Invalidated
    );

    // Delete + recreate with same SubscribeId produces new generation.
    assert!(store.delete_subscribe(sub_id).unwrap());
    insert_subscribe(&store, sub_id, None);
    let b4 = store.subscribe_policy_binding(sub_id).unwrap().unwrap();
    assert_ne!(b4.jobs_sync_generation, b3.jobs_sync_generation);
}

#[test]
fn download_and_boost_reads_do_not_wait_for_subscribe_writer() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let dl_id = DownloaderId::new();
    let sub_id = SubscribeId::new();
    insert_subscribe(&store, sub_id, None);
    store
        .accept_download_operation(&accept_input(
            "op-lock",
            "ik-lock",
            Some(sub_id),
            Some(dl_id),
        ))
        .unwrap();

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer_store = Arc::clone(&store);
    let writer_handle = std::thread::spawn(move || {
        writer_store
            .with_writer(Db::Subscribe, |_conn| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .unwrap();
    });
    entered_rx.recv().unwrap();

    let started = Instant::now();
    assert!(store.get_download_operation("op-lock").unwrap().is_some());
    assert!(
        store
            .list_download_operations(&DownloadOperationFilter::default(), None, Some(10))
            .unwrap()
            .items
            .len()
            == 1
    );
    assert!(store.get_boost_snapshot_facts("pool:x", &[dl_id]).is_ok());
    assert!(store.subscribe_policy_binding(sub_id).unwrap().is_some());
    assert!(store.list_due_subscribe_defers(100, None, Some(10)).is_ok());
    assert!(started.elapsed() < Duration::from_millis(200));

    release_tx.send(()).unwrap();
    writer_handle.join().unwrap();
}

#[test]
fn commit_boost_final_reservation_populates_null_downloader_id_and_rejects_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_1 = DownloaderId::new();
    let dl_2 = DownloaderId::new();

    let acc_null = store
        .accept_download_operation(&accept_input("op-null-dl", "ik-null-dl", None, None))
        .unwrap();
    assert_eq!(acc_null.operation.downloader_id, None);
    let prov = store
        .reserve_boost_provisional(&ReserveBoostProvisionalInput {
            reservation_id: "res-null-dl".into(),
            operation_id: "op-null-dl".into(),
            quota_key: "pool:main".into(),
            plugin_id: "builtin.boost".into(),
            site_id: None,
            downloader_id: dl_1,
            info_hash: Some("hash-null-dl".into()),
            estimated_bytes: 1_000,
            expires_at: None,
            now_utc: 100,
        })
        .unwrap();
    let committed = store
        .commit_boost_final_reservation(&CommitBoostFinalInput {
            operation_id: "op-null-dl".into(),
            expected_reservation_revision: prov.revision,
            info_hash: "hash-null-dl".into(),
            bytes_digest: "dig-null-dl".into(),
            verified_bytes: 1_500,
            budget_bytes: 10_000,
            max_add_per_hour: None,
            mark_operation_submitting: true,
            now_utc: 200,
        })
        .unwrap();
    assert_eq!(committed.stage, BoostReservationStage::Final);
    let op_after = store.get_download_operation("op-null-dl").unwrap().unwrap();
    assert_eq!(op_after.state, DownloadOperationState::Submitting);
    assert_eq!(op_after.downloader_id, Some(dl_1));

    // Mismatched non-null downloader_id on download_operations must fail closed with Conflict.
    store
        .accept_download_operation(&accept_input(
            "op-mismatch-dl",
            "ik-mismatch-dl",
            None,
            Some(dl_1),
        ))
        .unwrap();
    let prov_mismatch = store
        .reserve_boost_provisional(&ReserveBoostProvisionalInput {
            reservation_id: "res-mismatch-dl".into(),
            operation_id: "op-mismatch-dl".into(),
            quota_key: "pool:main".into(),
            plugin_id: "builtin.boost".into(),
            site_id: None,
            downloader_id: dl_2,
            info_hash: Some("hash-mismatch-dl".into()),
            estimated_bytes: 1_000,
            expires_at: None,
            now_utc: 100,
        })
        .unwrap();
    let err = store.commit_boost_final_reservation(&CommitBoostFinalInput {
        operation_id: "op-mismatch-dl".into(),
        expected_reservation_revision: prov_mismatch.revision,
        info_hash: "hash-mismatch-dl".into(),
        bytes_digest: "dig-mismatch-dl".into(),
        verified_bytes: 1_500,
        budget_bytes: 10_000,
        max_add_per_hour: None,
        mark_operation_submitting: true,
        now_utc: 200,
    });
    assert!(matches!(
        err,
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    let op_mismatch = store
        .get_download_operation("op-mismatch-dl")
        .unwrap()
        .unwrap();
    assert_eq!(op_mismatch.state, DownloadOperationState::Intent);
    assert_eq!(op_mismatch.downloader_id, Some(dl_1));
}

#[test]
fn mark_boost_task_retained_rejects_cleaned_cleanup_planned_and_retained_only() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    let r1 = seed_unknown_held_boost_task(&store, dl_id);

    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: "crawler-media-owned-op-r1".into(),
            info_hash: Some("hash-r1".into()),
            operation_id: "op-r1".into(),
            subscribe_id: None,
            plugin_id: Some("builtin.boost".into()),
            subject: "subject:1".into(),
            site_id: None,
            enclosure: "https://pt.example/op-r1.torrent".into(),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::ReconciledOperation,
            verified_bytes: Some(2_000),
            bytes_digest: Some("dig-r1".into()),
            now_utc: 400,
        })
        .unwrap();
    let (_res, tf) = store
        .finalize_boost_reservation_to_task(&FinalizeBoostTaskInput {
            operation_id: "op-r1".into(),
            expected_reservation_revision: r1.revision,
            task_fact: UpsertBoostTaskFactInput {
                task_fact_id: "tf-ret-guard".into(),
                plugin_id: "builtin.boost".into(),
                quota_key: "pool:main".into(),
                downloader_id: dl_id,
                ownership_tag: "crawler-media-owned-op-r1".into(),
                info_hash: "hash-r1".into(),
                site_id: None,
                state: BoostTaskState::Seeding,
                occupied_bytes: 2_000,
                retained_bytes: 0,
                hr_active: Some(false),
                hr_until: None,
                hold_until: Some(500),
                free_until: None,
                protection_state: BoostProtectionState::Eligible,
                protection_reason: None,
                observed_at: 400,
                expected_revision: None,
                now_utc: 400,
            },
        })
        .unwrap();

    let retained = store
        .mark_boost_task_retained("tf-ret-guard", tf.revision, 2_000, 550)
        .unwrap();
    assert_eq!(retained.state, BoostTaskState::RetainedOnly);

    // Calling mark_boost_task_retained on RetainedOnly must fail with Conflict.
    let err_retained =
        store.mark_boost_task_retained("tf-ret-guard", retained.revision, 3_000, 560);
    assert!(matches!(
        err_retained,
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    let still_retained = store.get_boost_task_fact("tf-ret-guard").unwrap().unwrap();
    assert_eq!(still_retained.state, BoostTaskState::RetainedOnly);
    assert_eq!(still_retained.retained_bytes, 2_000);

    // Calling mark_boost_task_retained on CleanupPlanned must fail with Conflict.
    let planned = store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: "tf-ret-guard".into(),
            expected_revision: still_retained.revision,
            cleanup_plan_handle: "plan-ret-guard".into(),
            cleanup_plan_json: "{\"paths\":[\"/a\"]}".into(),
            ttl_secs: 600,
            now_utc: 600,
        })
        .unwrap();
    let err_planned = store.mark_boost_task_retained("tf-ret-guard", planned.revision, 4_000, 610);
    assert!(matches!(
        err_planned,
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    let still_planned = store.get_boost_task_fact("tf-ret-guard").unwrap().unwrap();
    assert_eq!(still_planned.state, BoostTaskState::CleanupPlanned);
    assert_eq!(still_planned.retained_bytes, 2_000);

    // Calling mark_boost_task_retained on Cleaned must fail with Conflict and keep bytes at 0.
    let cleaned = store
        .complete_boost_cleanup("plan-ret-guard", still_planned.revision, 650)
        .unwrap();
    assert_eq!(cleaned.state, BoostTaskState::Cleaned);
    let err_cleaned = store.mark_boost_task_retained("tf-ret-guard", cleaned.revision, 5_000, 700);
    assert!(matches!(
        err_cleaned,
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    let still_cleaned = store.get_boost_task_fact("tf-ret-guard").unwrap().unwrap();
    assert_eq!(still_cleaned.state, BoostTaskState::Cleaned);
    assert_eq!(still_cleaned.retained_bytes, 0);
    assert_eq!(still_cleaned.occupied_bytes, 0);
}
