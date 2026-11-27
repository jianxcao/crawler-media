use domain::{DownloaderId, SubscribeId};
use store::Store;
use store::StoreError;
use store::download_operations::*;
use store::plugins::PluginError;

use super::{accept_input, insert_subscribe};

fn advance_op_to(
    store: &Store,
    dl_id: DownloaderId,
    op_id: &str,
    idx: usize,
    op_rev: i64,
    state: DownloadOperationState,
) {
    if state == DownloadOperationState::Deferred {
        store
            .transition_download_operation(&TransitionDownloadOperationInput {
                operation_id: op_id.into(),
                expected_revision: op_rev,
                next_state: DownloadOperationState::Deferred,
                downloader_id: Some(dl_id),
                endpoint_fingerprint: None,
                info_hash: None,
                bytes_digest: None,
                verified_bytes: None,
                outcome_code: Some("defer".into()),
                error_message: None,
                submitted_at: None,
                now_utc: 110,
            })
            .unwrap();
        return;
    }
    let sub = store
        .transition_download_operation(&TransitionDownloadOperationInput {
            operation_id: op_id.into(),
            expected_revision: op_rev,
            next_state: DownloadOperationState::Submitting,
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            info_hash: Some(format!("hash-bad-{idx}")),
            bytes_digest: Some(format!("dig-bad-{idx}")),
            verified_bytes: Some(1_000),
            outcome_code: None,
            error_message: None,
            submitted_at: None,
            now_utc: 110,
        })
        .unwrap();
    match state {
        DownloadOperationState::Submitting => {}
        DownloadOperationState::Unknown => {
            store
                .transition_download_operation(&TransitionDownloadOperationInput {
                    operation_id: op_id.into(),
                    expected_revision: sub.revision,
                    next_state: DownloadOperationState::Unknown,
                    downloader_id: Some(dl_id),
                    endpoint_fingerprint: None,
                    info_hash: Some(format!("hash-bad-{idx}")),
                    bytes_digest: Some(format!("dig-bad-{idx}")),
                    verified_bytes: Some(1_000),
                    outcome_code: Some("timeout".into()),
                    error_message: None,
                    submitted_at: None,
                    now_utc: 120,
                })
                .unwrap();
        }
        DownloadOperationState::Submitted
        | DownloadOperationState::Rejected
        | DownloadOperationState::Failed => {
            store
                .reconcile_download_operation(&ReconcileDownloadOperationInput {
                    operation_id: op_id.into(),
                    expected_revision: sub.revision,
                    resolved_state: state,
                    info_hash: Some(format!("hash-bad-{idx}")),
                    outcome_code: Some("done".into()),
                    error_message: None,
                    ownership: None,
                    boost_task_fact: None,
                    now_utc: 120,
                })
                .unwrap();
        }
        _ => unreachable!(),
    }
}

#[test]
fn commit_boost_final_reservation_rejects_non_intent_or_reserved_operation_with_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();

    for (idx, state) in [
        DownloadOperationState::Submitting,
        DownloadOperationState::Deferred,
        DownloadOperationState::Submitted,
        DownloadOperationState::Rejected,
        DownloadOperationState::Failed,
        DownloadOperationState::Unknown,
    ]
    .into_iter()
    .enumerate()
    {
        let op_id = format!("op-bad-state-{idx}");
        let op = store
            .accept_download_operation(&accept_input(
                &op_id,
                &format!("ik-bad-state-{idx}"),
                None,
                Some(dl_id),
            ))
            .unwrap()
            .operation;
        let _prov = store
            .reserve_boost_provisional(&ReserveBoostProvisionalInput {
                reservation_id: format!("res-bad-state-{idx}"),
                operation_id: op_id.clone(),
                quota_key: "pool:main".into(),
                plugin_id: "builtin.boost".into(),
                site_id: None,
                downloader_id: dl_id,
                info_hash: Some(format!("hash-bad-{idx}")),
                estimated_bytes: 1_000,
                expires_at: None,
                now_utc: 100,
            })
            .unwrap();
        advance_op_to(&store, dl_id, &op_id, idx, op.revision, state);

        let res_before = store
            .get_boost_reservation_by_operation(&op_id)
            .unwrap()
            .unwrap();
        for mark_submitting in [true, false] {
            let err = store.commit_boost_final_reservation(&CommitBoostFinalInput {
                operation_id: op_id.clone(),
                expected_reservation_revision: res_before.revision,
                info_hash: format!("hash-bad-{idx}"),
                bytes_digest: format!("dig-bad-{idx}"),
                verified_bytes: 1_500,
                budget_bytes: 10_000,
                max_add_per_hour: None,
                mark_operation_submitting: mark_submitting,
                now_utc: 200,
            });
            assert!(
                matches!(err, Err(StoreError::Plugin(PluginError::Conflict))),
                "expected Conflict for op.state={state} mark_submitting={mark_submitting}, got {err:?}"
            );
        }
        assert_eq!(
            store
                .get_boost_reservation_by_operation(&op_id)
                .unwrap()
                .unwrap(),
            res_before
        );
    }
}

#[test]
fn transition_download_operation_blocks_terminal_exits_and_invalid_skips() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();

    let op_intent = store
        .accept_download_operation(&accept_input("op-skip", "ik-skip", None, Some(dl_id)))
        .unwrap()
        .operation;
    for skip_target in [
        DownloadOperationState::Submitted,
        DownloadOperationState::Unknown,
    ] {
        let err = store.transition_download_operation(&TransitionDownloadOperationInput {
            operation_id: "op-skip".into(),
            expected_revision: op_intent.revision,
            next_state: skip_target,
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            info_hash: Some("hash-skip".into()),
            bytes_digest: Some("dig-skip".into()),
            verified_bytes: Some(500),
            outcome_code: None,
            error_message: None,
            submitted_at: None,
            now_utc: 100,
        });
        assert!(matches!(
            err,
            Err(StoreError::Plugin(PluginError::Conflict))
        ));
    }

    for (idx, terminal_state) in [
        DownloadOperationState::Submitted,
        DownloadOperationState::Rejected,
        DownloadOperationState::Failed,
        DownloadOperationState::Deferred,
    ]
    .into_iter()
    .enumerate()
    {
        let op_id = format!("op-term-{idx}");
        let op = store
            .accept_download_operation(&accept_input(
                &op_id,
                &format!("ik-term-{idx}"),
                None,
                Some(dl_id),
            ))
            .unwrap()
            .operation;
        advance_op_to(
            &store,
            dl_id,
            &op_id,
            100 + idx,
            op.revision,
            terminal_state,
        );
        let term_op = store.get_download_operation(&op_id).unwrap().unwrap();
        let exit_err = store.transition_download_operation(&TransitionDownloadOperationInput {
            operation_id: op_id,
            expected_revision: term_op.revision,
            next_state: DownloadOperationState::Failed,
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            info_hash: None,
            bytes_digest: None,
            verified_bytes: None,
            outcome_code: None,
            error_message: None,
            submitted_at: None,
            now_utc: 130,
        });
        assert!(matches!(
            exit_err,
            Err(StoreError::Plugin(PluginError::Conflict))
        ));
    }
}

#[test]
fn accept_or_retry_supersedes_failed_and_deferred_while_blocking_mismatch_and_replaying_others() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();

    for (idx, state) in [
        DownloadOperationState::Failed,
        DownloadOperationState::Deferred,
    ]
    .into_iter()
    .enumerate()
    {
        let key = format!("ik-retry-{idx}");
        let op1_id = format!("op-retry-1-{idx}");
        let op1 = store
            .accept_or_retry_download_operation(&accept_input(&op1_id, &key, None, Some(dl_id)))
            .unwrap()
            .operation;
        advance_op_to(&store, dl_id, &op1_id, 200 + idx, op1.revision, state);

        let mut mismatch = accept_input(&format!("op-retry-mis-{idx}"), &key, None, Some(dl_id));
        mismatch.request_digest = "different-digest".into();
        assert!(matches!(
            store.accept_or_retry_download_operation(&mismatch),
            Err(StoreError::Plugin(PluginError::IdempotencyConflict))
        ));

        let op2_id = format!("op-retry-2-{idx}");
        let op2 = store
            .accept_or_retry_download_operation(&accept_input(&op2_id, &key, None, Some(dl_id)))
            .unwrap();
        assert!(!op2.replayed);
        assert_eq!(op2.operation.operation_id, op2_id);
        assert_eq!(op2.operation.state, DownloadOperationState::Intent);
        assert_eq!(
            store
                .get_download_operation(&op1_id)
                .unwrap()
                .unwrap()
                .state,
            state,
            "superseded operation must remain in {state} without leaving terminal state"
        );
    }

    let op_sub = store
        .accept_or_retry_download_operation(&accept_input("op-sub-1", "ik-sub", None, Some(dl_id)))
        .unwrap()
        .operation;
    advance_op_to(
        &store,
        dl_id,
        "op-sub-1",
        300,
        op_sub.revision,
        DownloadOperationState::Submitted,
    );
    let replay = store
        .accept_or_retry_download_operation(&accept_input("op-sub-2", "ik-sub", None, Some(dl_id)))
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.operation.operation_id, "op-sub-1");
    assert_eq!(replay.operation.state, DownloadOperationState::Submitted);
}

#[test]
fn update_nonoverlapping_subscribe_bumps_policy_revision_and_invalidates_defers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let sub_id = SubscribeId::new();
    let mut sub = insert_subscribe(&store, sub_id, None);
    let b1 = store.subscribe_policy_binding(sub_id).unwrap().unwrap();

    store
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: "def-patch-1".into(),
            subscribe_id: sub_id,
            subscribe_generation: b1.jobs_sync_generation.clone(),
            policy_revision: b1.policy_revision,
            enclosure: "https://pt.example/patch.torrent".into(),
            info_hash: None,
            plugin_id: Some("p1".into()),
            hook_id: "download.before".into(),
            reason: "wait".into(),
            next_retry_at: 50,
            now_utc: 10,
        })
        .unwrap();

    sub.wash_cut = true;
    store.update_nonoverlapping_subscribe(&sub).unwrap();
    let b2 = store.subscribe_policy_binding(sub_id).unwrap().unwrap();
    assert!(b2.policy_revision > b1.policy_revision);
    assert_eq!(
        store
            .get_subscribe_defer(sub_id, "https://pt.example/patch.torrent")
            .unwrap()
            .unwrap()
            .state,
        SubscribeDeferState::Invalidated
    );
}

#[test]
fn get_subscribe_defer_same_second_prefers_current_policy_revision_over_defer_id_tie() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let sub_id = SubscribeId::new();
    let mut sub = insert_subscribe(&store, sub_id, None);
    let b1 = store.subscribe_policy_binding(sub_id).unwrap().unwrap();
    let enc = "https://pt.example/same-sec.torrent";
    let same_sec = 1_700_000_000;

    store
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: "zzz-old".into(),
            subscribe_id: sub_id,
            subscribe_generation: b1.jobs_sync_generation.clone(),
            policy_revision: b1.policy_revision,
            enclosure: enc.into(),
            info_hash: None,
            plugin_id: Some("p1".into()),
            hook_id: "download.before".into(),
            reason: "wait-old".into(),
            next_retry_at: same_sec + 60,
            now_utc: same_sec,
        })
        .unwrap();

    sub.wash_cut = true;
    store.update_subscribe(&sub).unwrap();
    let b2 = store.subscribe_policy_binding(sub_id).unwrap().unwrap();
    assert!(b2.policy_revision > b1.policy_revision);

    store
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: "000-new".into(),
            subscribe_id: sub_id,
            subscribe_generation: b2.jobs_sync_generation,
            policy_revision: b2.policy_revision,
            enclosure: enc.into(),
            info_hash: None,
            plugin_id: Some("p1".into()),
            hook_id: "download.before".into(),
            reason: "wait-new".into(),
            next_retry_at: same_sec + 120,
            now_utc: same_sec,
        })
        .unwrap();

    let got = store.get_subscribe_defer(sub_id, enc).unwrap().unwrap();
    assert_eq!(got.defer_id, "000-new");
    assert_eq!(got.policy_revision, b2.policy_revision);
    assert_eq!(got.state, SubscribeDeferState::Deferred);
}
