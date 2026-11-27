#[path = "plugin_support/mod.rs"]
mod plugin_support;
use plugin_support::*;
use store::{ManualCredentialWrite, Store, StoreError, plugins::*};

fn chained_request(
    p: &PluginRecord,
    subject: PluginSubject,
    key: &str,
    cause: PluginCommandCause,
) -> AcceptPluginOperation {
    AcceptPluginOperation {
        cause,
        ..request(p, subject, key)
    }
}

#[test]
fn cause_chain_tracks_depth_to_four_and_rejects_cycles_and_depth_five() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p_a = enable(&store, &install(&store, "org.example.chain_a", &[]));
    let p_b = enable(&store, &install(&store, "org.example.chain_b", &[]));
    let p_c = enable(&store, &install(&store, "org.example.chain_c", &[]));
    let p_d = enable(&store, &install(&store, "org.example.chain_d", &[]));
    let p_e = enable(&store, &install(&store, "org.example.chain_e", &[]));
    let subject = PluginSubject::User(domain::UserId::new());
    let mut cause = PluginCommandCause::root("trace-chain-1");

    for (idx, plugin) in [&p_a, &p_b, &p_c, &p_d].into_iter().enumerate() {
        let state = store.issue_plugin_state_authority(plugin, None).unwrap();
        let auth = fixture_command(&store, &state, None);
        let accepted = store
            .accept_plugin_command(
                &auth,
                &chained_request(plugin, subject.clone(), &format!("hop-{idx}"), cause),
                b"step",
            )
            .unwrap();
        let claim = store
            .claim_plugin_command(
                accepted.operation.operation_id,
                accepted.operation.revision,
                accepted.operation.generation,
                0,
                10,
            )
            .unwrap();
        let inspected = store.inspect_claimed_plugin_command(&claim).unwrap();
        assert_eq!(inspected.cause.depth as usize, idx + 1);
        assert_eq!(inspected.cause.visited.len(), idx + 1);
        cause = inspected.cause;
    }

    let state_e = store.issue_plugin_state_authority(&p_e, None).unwrap();
    let auth_e = fixture_command(&store, &state_e, None);
    let too_deep = store.accept_plugin_command(
        &auth_e,
        &chained_request(&p_e, subject.clone(), "hop-4", cause.clone()),
        b"too-deep",
    );
    assert!(matches!(
        too_deep,
        Err(StoreError::Plugin(PluginError::CauseLoop))
    ));

    // Reusing an earlier (plugin_id, kind, target_key) tuple at depth < 4 is a cycle.
    let state_b = store.issue_plugin_state_authority(&p_b, None).unwrap();
    let auth_b = fixture_command(&store, &state_b, None);
    let loop_cause = PluginCommandCause {
        cause_id: cause.cause_id.clone(),
        depth: 2,
        visited: cause.visited[..2].to_vec(),
    };
    assert!(matches!(
        store.accept_plugin_command(
            &auth_b,
            &chained_request(&p_b, subject.clone(), "cycle-b", loop_cause.clone()),
            b"cycle",
        ),
        Err(StoreError::Plugin(PluginError::CauseLoop))
    ));

    // A distinct plugin_id or target_key at the same depth is allowed.
    let diff = store
        .accept_plugin_command(
            &auth_e,
            &chained_request(&p_e, subject, "non-cycle-e", loop_cause),
            b"ok",
        )
        .unwrap();
    assert_eq!(diff.operation.state, PluginOperationState::Queued);
}

#[test]
fn stale_authority_is_rejected_before_and_after_claim_and_cannot_publish_success() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let s = site();
    store.insert_site(&s).unwrap();
    let p = enable(
        &store,
        &install(&store, "org.example.stale_cmd", std::slice::from_ref(&s)),
    );
    let state = store
        .issue_plugin_state_authority(&p, store.observe_site(s.id).unwrap().as_ref())
        .unwrap();
    let auth = fixture_command(&store, &state, Some(s.id));
    let subject = PluginSubject::User(domain::UserId::new());
    let op1 = store
        .accept_plugin_command(&auth, &request(&p, subject.clone(), "stale-1"), b"p1")
        .unwrap()
        .operation;
    let op2 = store
        .accept_plugin_command(&auth, &request(&p, subject.clone(), "stale-2"), b"p2")
        .unwrap()
        .operation;
    let claim2 = store
        .claim_plugin_command(op2.operation_id, op2.revision, op2.generation, 0, 10)
        .unwrap();

    // Rotate site credential_epoch while op1 is queued and op2 is running.
    store.save_site(&s, ManualCredentialWrite::Replace).unwrap();

    assert!(matches!(
        store.claim_plugin_command(op1.operation_id, op1.revision, op1.generation, 0, 11),
        Err(StoreError::Plugin(PluginError::StaleAuthority))
    ));
    let rejected1 = store
        .reject_stale_plugin_command(op1.operation_id, op1.revision, op1.generation, 0, 11)
        .unwrap();
    assert_eq!(rejected1.state, PluginOperationState::Rejected);
    assert_eq!(rejected1.phase, "authority-rejected");

    assert!(matches!(
        store.inspect_claimed_plugin_command(&claim2),
        Err(StoreError::Plugin(PluginError::StaleAuthority))
    ));
    assert!(matches!(
        store.record_plugin_command_outcome(&claim2, &outcome(PluginOperationState::Succeeded)),
        Err(StoreError::Plugin(PluginError::StaleAuthority))
    ));
    let rejected2 = store
        .record_plugin_command_outcome(
            &claim2,
            &PluginCommandOutcome {
                state: PluginOperationState::Rejected,
                phase: "authority-rejected".into(),
                next_run_at: None,
                result: None,
                effect: PluginEffectObservation::NoEffect,
            },
        )
        .unwrap();
    assert_eq!(rejected2.state, PluginOperationState::Rejected);

    let status = store
        .plugin_command_status(op2.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(status.operation.state, PluginOperationState::Rejected);
    assert_eq!(status.attempt, 1);

    // Task binding lookup by ID returns persisted definition.
    let binding = store
        .save_plugin_task_binding(
            &p,
            "sync",
            0,
            &PluginTaskDefinition {
                enabled: true,
                interval_secs: Some(300),
                public_config: serde_json::json!({"site_id": s.id}),
            },
        )
        .unwrap();
    assert_eq!(
        store
            .get_plugin_task_binding_by_id(binding.binding_id)
            .unwrap(),
        Some(binding)
    );
}
