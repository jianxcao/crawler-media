#[path = "plugin_support/mod.rs"]
mod plugin_support;
use plugin_support::*;
use store::{ManualCredentialWrite, Store, StoreError, plugins::*};

#[test]
fn runtime_payloads_and_results_preserve_domain_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (a, b) = (site(), site());
    store.insert_site(&a).unwrap();
    store.insert_site(&b).unwrap();
    let p = enable(
        &store,
        &install(&store, "org.example.bound", &[a.clone(), b.clone()]),
    );
    let aa = store
        .issue_plugin_state_authority(&p, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    let bb = store
        .issue_plugin_state_authority(&p, store.observe_site(b.id).unwrap().as_ref())
        .unwrap();
    let global = enable(&store, &install(&store, "org.example.global", &[]));
    let global = store.issue_plugin_state_authority(&global, None).unwrap();
    let subject = PluginSubject::EndpointKey {
        key_id: uuid::Uuid::new_v4(),
        issuer_user_id: domain::UserId::new(),
    };
    let input = request(&p, subject.clone(), "result");
    let command_a = fixture_command(&store, &aa, Some(a.id));
    let command_b = fixture_command(&store, &bb, Some(b.id));
    let op = store
        .accept_plugin_command(&command_a, &input, b"private-payload-a")
        .unwrap()
        .operation;
    assert!(matches!(
        store.accept_plugin_command(&command_b, &input, b"private-payload-b"),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
    let claim = store
        .claim_plugin_command(op.operation_id, op.revision, op.generation, 0, 1)
        .unwrap();
    assert_eq!(
        store
            .plugin_command_payload(&claim)
            .unwrap()
            .unwrap()
            .security_scope(),
        aa.security_scope()
    );
    let mut output = outcome(PluginOperationState::Succeeded);
    output.result = Some(b"private-result-a".to_vec());
    let projection = store
        .record_plugin_command_outcome(&claim, &output)
        .unwrap();
    assert!(!format!("{projection:?}").contains("private-result-a"));
    assert_eq!(
        store
            .plugin_operation_result(&bb, &subject, op.operation_id)
            .unwrap(),
        None
    );
    assert!(
        store
            .plugin_operation_result(&global, &subject, op.operation_id)
            .is_err()
    );
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(
        reopened
            .plugin_operation_result(&bb, &subject, op.operation_id)
            .unwrap(),
        None
    );
    let bytes = reopened
        .plugin_operation_result(&aa, &subject, op.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(bytes.as_bytes(), b"private-result-a");
    assert_eq!(bytes.security_scope(), aa.security_scope());
    let stranger = PluginSubject::User(domain::UserId::new());
    assert_eq!(
        reopened
            .get_plugin_operation(&stranger, op.operation_id)
            .unwrap(),
        None
    );
    assert!(
        reopened
            .plugin_operation_result(&aa, &stranger, op.operation_id)
            .is_err()
    );
    store.save_site(&a, ManualCredentialWrite::Replace).unwrap();
    assert!(matches!(
        reopened.plugin_operation_result(&aa, &subject, op.operation_id),
        Err(StoreError::Plugin(PluginError::StaleAuthority))
    ));
    let new = store
        .issue_plugin_state_authority(&p, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    assert_eq!(
        store
            .plugin_operation_result(&new, &subject, op.operation_id)
            .unwrap(),
        None
    );
}

#[test]
fn scoped_management_results_need_current_disabled_admin_authority() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (a, b) = (site(), site());
    store.insert_site(&a).unwrap();
    store.insert_site(&b).unwrap();
    let p = install(&store, "org.example.bound", &[a.clone(), b.clone()]);
    let aa = store
        .issue_plugin_config_authority(&p, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    let bb = store
        .issue_plugin_config_authority(&p, store.observe_site(b.id).unwrap().as_ref())
        .unwrap();
    let subject = PluginSubject::User(domain::UserId::new());
    let op = store
        .accept_plugin_config_operation(
            &aa,
            &request(&p, subject.clone(), "admin-result"),
            b"private-admin-payload",
        )
        .unwrap()
        .operation;
    assert_eq!(op.generation, None);
    let claim = store
        .claim_plugin_command(op.operation_id, op.revision, None, 0, 1)
        .unwrap();
    assert_eq!(
        store
            .plugin_command_payload(&claim)
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"private-admin-payload"
    );
    let mut output = outcome(PluginOperationState::Succeeded);
    output.result = Some(b"private-admin-result".to_vec());
    store
        .record_plugin_command_outcome(&claim, &output)
        .unwrap();
    assert_eq!(
        store
            .export_plugin_config_operation_result(&bb, &subject, op.operation_id)
            .unwrap(),
        None
    );
    assert_eq!(
        Store::open(dir.path())
            .unwrap()
            .export_plugin_config_operation_result(&aa, &subject, op.operation_id)
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"private-admin-result"
    );
    let updated = store
        .save_plugin_config(&p.plugin_id, p.revision, "2", &serde_json::json!({}))
        .unwrap();
    assert!(
        store
            .export_plugin_config_operation_result(&aa, &subject, op.operation_id)
            .is_err()
    );
    let new = store
        .issue_plugin_config_authority(&updated, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    assert!(
        store
            .export_plugin_config_operation_result(&new, &subject, op.operation_id)
            .unwrap()
            .is_some()
    );
    let enabled = enable(&store, &updated);
    assert!(
        store
            .issue_plugin_config_authority(&enabled, store.observe_site(a.id).unwrap().as_ref())
            .is_err()
    );
}

#[test]
fn verified_refresh_preserves_publication_but_manual_epoch_cannot_be_blessed_at_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = site();
    store.insert_site(&a).unwrap();
    let p = enable(
        &store,
        &install(&store, "org.example.bound", std::slice::from_ref(&a)),
    );
    let observation = store.observe_site(a.id).unwrap().unwrap();
    let aa = store
        .issue_plugin_state_authority(&p, Some(&observation))
        .unwrap();
    let command_a = fixture_command(&store, &aa, Some(a.id));
    let subject = PluginSubject::User(domain::UserId::new());
    let op = store
        .accept_plugin_command(&command_a, &request(&p, subject.clone(), "refresh"), b"")
        .unwrap()
        .operation;
    let claim = store
        .claim_plugin_command(op.operation_id, op.revision, op.generation, 0, 1)
        .unwrap();
    assert!(
        store
            .save_site_if_current(&observation, Some("verified-cookie"))
            .unwrap()
    );
    let mut output = outcome(PluginOperationState::Succeeded);
    output.result = Some(b"same-domain".to_vec());
    store
        .record_plugin_command_outcome(&claim, &output)
        .unwrap();
    let op = store
        .accept_plugin_command(&command_a, &request(&p, subject.clone(), "manual"), b"")
        .unwrap()
        .operation;
    let stale = store
        .claim_plugin_command(op.operation_id, op.revision, op.generation, 0, 1)
        .unwrap();
    store.save_site(&a, ManualCredentialWrite::Replace).unwrap();
    assert!(matches!(
        store.record_plugin_command_outcome(&stale, &output),
        Err(StoreError::Plugin(PluginError::StaleAuthority))
    ));
    let current = store
        .issue_plugin_state_authority(&p, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    assert_eq!(
        store
            .plugin_operation_result(&current, &subject, op.operation_id)
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .get_plugin_operation(&subject, op.operation_id)
            .unwrap()
            .unwrap()
            .state,
        PluginOperationState::Running
    );
    let unknown = store
        .record_plugin_command_interruption(
            op.operation_id,
            stale.revision(),
            stale.generation(),
            stale.attempt(),
        )
        .unwrap();
    assert_eq!(unknown.state, PluginOperationState::Unknown);
}

#[test]
fn admin_operation_read_redacts_result_json_when_site_credential_epoch_rotates() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = site();
    store.insert_site(&a).unwrap();
    let p = enable(
        &store,
        &install(&store, "org.example.bound", std::slice::from_ref(&a)),
    );
    let aa = store
        .issue_plugin_state_authority(&p, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    let command_a = fixture_command(&store, &aa, Some(a.id));
    let subject = PluginSubject::User(domain::UserId::new());
    let op = store
        .accept_plugin_command(&command_a, &request(&p, subject, "admin-epoch"), b"")
        .unwrap()
        .operation;
    let claim = store
        .claim_plugin_command(op.operation_id, op.revision, op.generation, 0, 1)
        .unwrap();
    let mut output = outcome(PluginOperationState::Succeeded);
    output.result = Some(br#"{"cookie_echo":"epoch-1"}"#.to_vec());
    store
        .record_plugin_command_outcome(&claim, &output)
        .unwrap();

    let before = store
        .get_plugin_operation_admin(op.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        before.result_json,
        Some(serde_json::json!({"cookie_echo": "epoch-1"}))
    );

    store.save_site(&a, ManualCredentialWrite::Replace).unwrap();

    let after = store
        .get_plugin_operation_admin(op.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(after.operation.operation_id, op.operation_id);
    assert_eq!(after.result_json, None);
}
