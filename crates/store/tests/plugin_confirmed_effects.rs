#[path = "plugin_support/mod.rs"]
mod plugin_support;
use plugin_support::*;
use store::{Store, StoreError, plugins::*};

fn confirmed(
    store: &Store,
    p: &PluginRecord,
    authority: &PluginCommandAuthority,
    subject: &PluginSubject,
    key: &str,
    state: PluginOperationState,
) -> (PluginOperation, i64) {
    let op = store
        .accept_plugin_command(authority, &request(p, subject.clone(), key), b"")
        .unwrap()
        .operation;
    let claim = store
        .claim_plugin_command(op.operation_id, op.revision, op.generation, 0, 1)
        .unwrap();
    let mut result = outcome(state);
    result.effect = PluginEffectObservation::Confirmed;
    if state == PluginOperationState::Deferred {
        result.next_run_at = Some(2);
    }
    let op = store
        .record_plugin_command_outcome(&claim, &result)
        .unwrap();
    assert!(
        store
            .record_plugin_command_outcome(&claim, &outcome(PluginOperationState::Cancelled))
            .is_err()
    );
    (op, claim.attempt())
}
fn must_preserve(store: &Store, subject: &PluginSubject, op: &PluginOperation, attempt: i64) {
    assert_eq!(op.effect, PluginEffectObservation::Confirmed);
    assert!(matches!(
        store.cancel_plugin_operation(subject, op.operation_id, op.revision),
        Err(StoreError::Plugin(PluginError::EffectDispositionRequired))
    ));
    assert!(matches!(
        store.resume_plugin_command(subject, op.operation_id, op.revision),
        Err(StoreError::Plugin(PluginError::EffectDispositionRequired))
    ));
    assert!(matches!(
        store.claim_plugin_command(op.operation_id, op.revision, op.generation, attempt, 100),
        Err(StoreError::Plugin(PluginError::EffectDispositionRequired))
    ));
    assert!(matches!(
        store.reconcile_plugin_command(&PluginReconciliationObservation {
            operation_id: op.operation_id,
            revision: op.revision,
            generation: op.generation,
            attempt,
            outcome: PluginReconciliationOutcome::ConfirmedNoEffect
        }),
        Err(StoreError::Plugin(PluginError::EffectDispositionRequired))
    ));
    assert_eq!(
        store
            .get_plugin_operation(subject, op.operation_id)
            .unwrap(),
        Some(op.clone())
    );
    assert!(
        store
            .pending_plugin_commands(None, None, 100)
            .unwrap()
            .iter()
            .all(|pending| pending.operation.operation_id != op.operation_id)
    );
    assert!(
        store
            .unresolved_plugin_commands(None, None)
            .unwrap()
            .iter()
            .any(|pending| pending.operation == *op)
    );
}

#[test]
fn confirmed_waiting_blocked_and_deferred_require_disposition_without_automatic_execution() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p = enable(&store, &install(&store, "org.example.effects", &[]));
    let authority = fixture_command(
        &store,
        &store.issue_plugin_state_authority(&p, None).unwrap(),
        None,
    );
    let subject = PluginSubject::User(domain::UserId::new());
    for (index, state) in [
        PluginOperationState::Waiting,
        PluginOperationState::Blocked,
        PluginOperationState::Deferred,
    ]
    .into_iter()
    .enumerate()
    {
        let (op, attempt) = confirmed(
            &store,
            &p,
            &authority,
            &subject,
            &format!("state-{index}"),
            state,
        );
        if state == PluginOperationState::Deferred {
            assert_eq!(op.state, PluginOperationState::Blocked);
            assert_eq!(op.phase, "effect-disposition-required");
            assert_eq!(op.next_run_at, None);
        } else {
            assert_eq!(op.state, state);
        }
        must_preserve(&store, &subject, &op, attempt);
        let mut observation = PluginReconciliationObservation {
            operation_id: op.operation_id,
            revision: op.revision - 1,
            generation: op.generation,
            attempt,
            outcome: PluginReconciliationOutcome::Succeeded,
        };
        assert!(store.reconcile_plugin_command(&observation).is_err());
        observation.revision = op.revision;
        if index == 1 {
            observation.outcome = PluginReconciliationOutcome::Failed;
        }
        let settled = store.reconcile_plugin_command(&observation).unwrap();
        assert!(settled.state.is_terminal());
        assert_eq!(settled.effect, PluginEffectObservation::Confirmed);
        assert_eq!(
            Store::open(dir.path())
                .unwrap()
                .get_plugin_operation(&subject, op.operation_id)
                .unwrap(),
            Some(settled)
        );
    }
}

#[test]
fn disable_preserves_confirmed_nonterminal_effects_and_drain_waits_for_fenced_settlement() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p = enable(&store, &install(&store, "org.example.effects", &[]));
    let authority = fixture_command(
        &store,
        &store.issue_plugin_state_authority(&p, None).unwrap(),
        None,
    );
    let subject = PluginSubject::User(domain::UserId::new());
    let original: Vec<_> = [
        PluginOperationState::Waiting,
        PluginOperationState::Blocked,
        PluginOperationState::Deferred,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, state)| {
        confirmed(
            &store,
            &p,
            &authority,
            &subject,
            &format!("disable-{index}"),
            state,
        )
    })
    .collect();
    let draining = store
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), false)
        .unwrap();
    let reopened = Store::open(dir.path()).unwrap();
    assert!(matches!(
        reopened.finalize_plugin_lifecycle(&evidence(&draining)),
        Err(StoreError::Plugin(PluginError::NotDrained))
    ));
    for (old, attempt) in original {
        let op = reopened
            .get_plugin_operation(&subject, old.operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(op.state, PluginOperationState::Blocked);
        assert!(op.revision > old.revision);
        assert_eq!(op.generation, old.generation);
        must_preserve(&reopened, &subject, &op, attempt);
        reopened
            .reconcile_plugin_command(&PluginReconciliationObservation {
                operation_id: op.operation_id,
                revision: op.revision,
                generation: op.generation,
                attempt,
                outcome: PluginReconciliationOutcome::Succeeded,
            })
            .unwrap();
    }
    assert_eq!(
        reopened
            .finalize_plugin_lifecycle(&evidence(&draining))
            .unwrap()
            .lifecycle,
        PluginLifecycle::Disabled
    );
}
