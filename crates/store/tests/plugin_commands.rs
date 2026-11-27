#[path = "plugin_support/mod.rs"]
mod plugin_support;
use plugin_support::*;
use store::{Store, StoreError, plugins::*};

#[test]
fn deferred_attempts_fence_old_workers_and_unknown_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p = enable(&store, &install(&store, "org.example.commands", &[]));
    let authority = store.issue_plugin_state_authority(&p, None).unwrap();
    let authority = fixture_command(&store, &authority, None);
    let subject = PluginSubject::Task {
        plugin_id: p.plugin_id.clone(),
        task_id: "fixture".into(),
    };
    let accepted = store
        .accept_plugin_command(
            &authority,
            &request(&p, subject.clone(), "one"),
            b"private-command",
        )
        .unwrap();
    let op = accepted.operation;
    assert_eq!(op.generation, Some(p.generation));
    assert!(
        store
            .claim_plugin_command(op.operation_id, op.revision, None, 0, 10)
            .is_err()
    );
    assert!(
        store
            .claim_plugin_command(op.operation_id, op.revision, op.generation, 1, 10)
            .is_err()
    );
    let first = store
        .claim_plugin_command(op.operation_id, op.revision, op.generation, 0, 10)
        .unwrap();
    assert_eq!(
        store
            .plugin_command_payload(&first)
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"private-command"
    );
    let mut deferred = outcome(PluginOperationState::Deferred);
    deferred.next_run_at = Some(20);
    let op = store
        .record_plugin_command_outcome(&first, &deferred)
        .unwrap();
    assert_eq!(op.next_run_at, Some(20));
    assert!(
        store
            .pending_plugin_commands(None, None, 19)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .claim_plugin_command(op.operation_id, op.revision, op.generation, 1, 19)
            .is_err()
    );
    assert!(
        store
            .record_plugin_command_outcome(&first, &outcome(PluginOperationState::Succeeded))
            .is_err()
    );
    let second = store
        .claim_plugin_command(op.operation_id, op.revision, op.generation, 1, 20)
        .unwrap();
    assert_eq!(second.attempt(), 2);
    let op = store
        .record_plugin_command_outcome(&second, &outcome(PluginOperationState::Unknown))
        .unwrap();
    assert!(!op.state.is_terminal());
    let reopened = Store::open(dir.path()).unwrap();
    assert!(
        reopened
            .pending_plugin_commands(None, None, 30)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        reopened
            .unresolved_plugin_commands(None, Some(200))
            .unwrap()[0]
            .operation,
        op
    );
    assert_eq!(
        reopened
            .get_plugin_operation(&subject, op.operation_id)
            .unwrap(),
        Some(op.clone())
    );
    assert!(matches!(
        reopened.cancel_plugin_operation(&subject, op.operation_id, op.revision),
        Err(StoreError::Plugin(PluginError::UnknownEffect))
    ));
    assert!(matches!(
        reopened.resume_plugin_command(&subject, op.operation_id, op.revision),
        Err(StoreError::Plugin(PluginError::UnknownEffect))
    ));
    assert!(
        reopened
            .claim_plugin_command(op.operation_id, op.revision, op.generation, 2, 30)
            .is_err()
    );
    let mut observed = PluginReconciliationObservation {
        operation_id: op.operation_id,
        revision: op.revision,
        generation: op.generation,
        attempt: 1,
        outcome: PluginReconciliationOutcome::ConfirmedNoEffect,
    };
    assert!(reopened.reconcile_plugin_command(&observed).is_err());
    observed.attempt = 2;
    assert_eq!(
        reopened.reconcile_plugin_command(&observed).unwrap().state,
        PluginOperationState::Cancelled
    );
}

#[test]
fn only_four_operation_states_are_terminal() {
    for state in [
        PluginOperationState::Queued,
        PluginOperationState::Running,
        PluginOperationState::Waiting,
        PluginOperationState::Blocked,
        PluginOperationState::Deferred,
        PluginOperationState::Unknown,
    ] {
        assert!(!state.is_terminal());
    }
    for state in [
        PluginOperationState::Succeeded,
        PluginOperationState::Rejected,
        PluginOperationState::Failed,
        PluginOperationState::Cancelled,
    ] {
        assert!(state.is_terminal());
    }
}

#[test]
fn disable_fences_claims_cancels_pending_and_persisted_unknown_prevents_false_drain() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p = enable(&store, &install(&store, "org.example.commands", &[]));
    let authority = store.issue_plugin_state_authority(&p, None).unwrap();
    let authority = fixture_command(&store, &authority, None);
    let subject = PluginSubject::User(domain::UserId::new());
    let queued = store
        .accept_plugin_command(&authority, &request(&p, subject.clone(), "queued"), b"")
        .unwrap()
        .operation;
    let running = store
        .accept_plugin_command(&authority, &request(&p, subject.clone(), "running"), b"")
        .unwrap()
        .operation;
    let claim = store
        .claim_plugin_command(
            running.operation_id,
            running.revision,
            running.generation,
            0,
            1,
        )
        .unwrap();
    assert!(
        store
            .begin_plugin_lifecycle(&p.plugin_id, p.revision, running.operation_id, false)
            .is_err()
    );
    let p = store
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), false)
        .unwrap();
    assert!(
        store
            .claim_plugin_command(
                queued.operation_id,
                queued.revision,
                queued.generation,
                0,
                2
            )
            .is_err()
    );
    assert!(
        store
            .record_plugin_command_outcome(&claim, &outcome(PluginOperationState::Succeeded))
            .is_err()
    );
    assert_eq!(
        store
            .get_plugin_operation(&subject, queued.operation_id)
            .unwrap()
            .unwrap()
            .state,
        PluginOperationState::Cancelled
    );
    let unknown = store
        .get_plugin_operation(&subject, running.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(unknown.state, PluginOperationState::Unknown);
    let reopened = Store::open(dir.path()).unwrap();
    assert!(matches!(
        reopened.finalize_plugin_lifecycle(&evidence(&p)),
        Err(StoreError::Plugin(PluginError::NotDrained))
    ));
    reopened
        .reconcile_plugin_command(&PluginReconciliationObservation {
            operation_id: unknown.operation_id,
            revision: unknown.revision,
            generation: unknown.generation,
            attempt: claim.attempt(),
            outcome: PluginReconciliationOutcome::Succeeded,
        })
        .unwrap();
    let disabled = reopened.finalize_plugin_lifecycle(&evidence(&p)).unwrap();
    let enabled = enable(&reopened, &disabled);
    assert!(enabled.generation > p.generation);
    assert!(
        reopened
            .accept_plugin_command(
                &authority,
                &request(&enabled, subject, "old-authority"),
                b""
            )
            .is_err()
    );
}

#[test]
fn management_generation_none_is_a_distinct_path_and_discovery_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p = install(&store, "org.example.commands", &[]);
    let subject = PluginSubject::User(domain::UserId::new());
    let input = request(&p, subject.clone(), "management");
    let op = store.accept_plugin_operation(&input).unwrap().operation;
    assert_eq!(op.generation, None);
    let claim = store
        .claim_plugin_command(op.operation_id, op.revision, None, 0, 1)
        .unwrap();
    assert!(
        store
            .record_plugin_command_outcome(
                &claim,
                &PluginCommandOutcome {
                    state: PluginOperationState::Succeeded,
                    phase: "done".into(),
                    next_run_at: None,
                    result: Some(b"unscoped-secret".to_vec()),
                    effect: PluginEffectObservation::NoEffect
                }
            )
            .is_err()
    );
    store
        .record_plugin_command_outcome(&claim, &outcome(PluginOperationState::Succeeded))
        .unwrap();
    for i in 0..205 {
        let mut input = input.clone();
        input.idempotency_key = format!("pending-{i:03}");
        store.accept_plugin_operation(&input).unwrap();
    }
    assert_eq!(
        store
            .pending_plugin_commands(None, None, 100)
            .unwrap()
            .len(),
        100
    );
    let first = store.pending_plugin_commands(None, Some(999), 100).unwrap();
    assert_eq!(first.len(), 200);
    let recovery = store.unresolved_plugin_commands(None, Some(999)).unwrap();
    assert_eq!(recovery.len(), 200);
    assert_eq!(
        store
            .unresolved_plugin_commands(
                Some(recovery.last().unwrap().operation.operation_id),
                Some(200)
            )
            .unwrap()
            .len(),
        5
    );
    let rest = store
        .pending_plugin_commands(Some(&first.last().unwrap().cursor), Some(200), 100)
        .unwrap();
    assert_eq!(rest.len(), 5);
    assert_eq!(
        store
            .list_plugin_operations(&subject, None, Some(999))
            .unwrap()
            .len(),
        200
    );
    let first = store
        .list_plugin_operations(&subject, None, Some(200))
        .unwrap();
    assert_eq!(
        store
            .list_plugin_operations(
                &subject,
                Some(first.last().unwrap().operation_id),
                Some(200)
            )
            .unwrap()
            .len(),
        6
    );
}
