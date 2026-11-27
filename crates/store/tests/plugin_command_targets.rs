#[path = "plugin_support/mod.rs"]
mod plugin_support;
use plugin_support::*;
use store::{Store, StoreError, plugins::*};

fn grant(kind: &str, id: impl ToString) -> PluginGrant {
    PluginGrant {
        capability: "command:fixture".into(),
        scope: PluginGrantScope::Object {
            kind: kind.into(),
            id: id.to_string(),
        },
        sensitive: false,
    }
}
fn registered(store: &Store, bound: bool, grants: &[PluginGrant]) -> PluginRecord {
    let p = store
        .register_plugin_version(&metadata("org.example.targets", bound), 0)
        .unwrap();
    let p = store
        .replace_plugin_grants(&p.plugin_id, &p.package_hash, p.revision, grants)
        .unwrap();
    enable(store, &p)
}

#[test]
fn global_data_domain_uses_exact_multiple_object_grants_without_global_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (a, b, c) = (
        domain::LibraryId::new(),
        domain::LibraryId::new(),
        domain::LibraryId::new(),
    );
    let p = registered(&store, false, &[grant("library", a), grant("library", b)]);
    let state = store.issue_plugin_state_authority(&p, None).unwrap();
    assert!(matches!(
        store.authorize_plugin_command(&state, "fixture", &[PluginCommandTarget::Global]),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
    assert!(matches!(
        store.authorize_plugin_command(
            &state,
            "fixture",
            &[
                PluginCommandTarget::Library(a),
                PluginCommandTarget::Library(c)
            ]
        ),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
    let targets = [
        PluginCommandTarget::Library(a),
        PluginCommandTarget::Library(b),
    ];
    let authority = store
        .authorize_plugin_command(&state, "fixture", &targets)
        .unwrap();
    assert_eq!(authority.security_scope(), state.security_scope());
    let subject = PluginSubject::User(domain::UserId::new());
    let input = request(&p, subject.clone(), "multi");
    let op = store
        .accept_plugin_command(&authority, &input, b"bounded-target-payload")
        .unwrap()
        .operation;
    let reordered = store
        .authorize_plugin_command(&state, "fixture", &[targets[1].clone(), targets[0].clone()])
        .unwrap();
    let replay = store
        .accept_plugin_command(&reordered, &input, b"bounded-target-payload")
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.operation.operation_id, op.operation_id);
    let subset = store
        .authorize_plugin_command(&state, "fixture", &[targets[0].clone()])
        .unwrap();
    assert!(matches!(
        store.accept_plugin_command(&subset, &input, b"bounded-target-payload"),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    let claim = store
        .claim_plugin_command(op.operation_id, op.revision, op.generation, 0, 1)
        .unwrap();
    let mut result = outcome(PluginOperationState::Succeeded);
    result.result = Some(b"scoped-result".to_vec());
    store
        .record_plugin_command_outcome(&claim, &result)
        .unwrap();
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(
        reopened
            .plugin_operation_result(&state, &subject, op.operation_id)
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"scoped-result"
    );
}

#[test]
fn site_data_domain_and_downloader_object_authorization_remain_independent() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (a, b) = (site(), site());
    store.insert_site(&a).unwrap();
    store.insert_site(&b).unwrap();
    let downloader = domain::DownloaderId::new();
    let p = registered(&store, true, &[grant("downloader", downloader)]);
    let aa = store
        .issue_plugin_state_authority(&p, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    let bb = store
        .issue_plugin_state_authority(&p, store.observe_site(b.id).unwrap().as_ref())
        .unwrap();
    let authority = store
        .authorize_plugin_command(
            &aa,
            "fixture",
            &[PluginCommandTarget::Downloader(downloader)],
        )
        .unwrap();
    assert!(matches!(
        store.authorize_plugin_command(
            &aa,
            "fixture",
            &[PluginCommandTarget::Downloader(domain::DownloaderId::new())]
        ),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
    let subject = PluginSubject::User(domain::UserId::new());
    let op = store
        .accept_plugin_command(
            &authority,
            &request(&p, subject.clone(), "downloader"),
            b"domain-a-payload",
        )
        .unwrap()
        .operation;
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
    let mut result = outcome(PluginOperationState::Succeeded);
    result.result = Some(b"domain-a-result".to_vec());
    store
        .record_plugin_command_outcome(&claim, &result)
        .unwrap();
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(
        reopened
            .plugin_operation_result(&bb, &subject, op.operation_id)
            .unwrap(),
        None
    );
    assert_eq!(
        reopened
            .plugin_operation_result(&aa, &subject, op.operation_id)
            .unwrap()
            .unwrap()
            .security_scope(),
        aa.security_scope()
    );
}

#[test]
fn revoked_object_grant_rejects_current_result_read_and_old_command_authority() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = domain::LibraryId::new();
    let mut p = registered(&store, false, &[grant("library", library)]);
    let state = store.issue_plugin_state_authority(&p, None).unwrap();
    let authority = store
        .authorize_plugin_command(&state, "fixture", &[PluginCommandTarget::Library(library)])
        .unwrap();
    let subject = PluginSubject::User(domain::UserId::new());
    let completed = store
        .accept_plugin_command(&authority, &request(&p, subject.clone(), "complete"), b"")
        .unwrap()
        .operation;
    let first = store
        .claim_plugin_command(
            completed.operation_id,
            completed.revision,
            completed.generation,
            0,
            1,
        )
        .unwrap();
    let mut result = outcome(PluginOperationState::Succeeded);
    result.result = Some(b"old-object-result".to_vec());
    store
        .record_plugin_command_outcome(&first, &result)
        .unwrap();
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
    p = store
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), false)
        .unwrap();
    assert!(
        store
            .record_plugin_command_outcome(&claim, &result)
            .is_err()
    );
    let unknown = store
        .get_plugin_operation(&subject, running.operation_id)
        .unwrap()
        .unwrap();
    store
        .reconcile_plugin_command(&PluginReconciliationObservation {
            operation_id: unknown.operation_id,
            revision: unknown.revision,
            generation: unknown.generation,
            attempt: claim.attempt(),
            outcome: PluginReconciliationOutcome::ConfirmedNoEffect,
        })
        .unwrap();
    p = store.finalize_plugin_lifecycle(&evidence(&p)).unwrap();
    p = store
        .replace_plugin_grants(&p.plugin_id, &p.package_hash, p.revision, &[])
        .unwrap();
    p = enable(&store, &p);
    let current = store.issue_plugin_state_authority(&p, None).unwrap();
    assert!(matches!(
        store.authorize_plugin_command(
            &current,
            "fixture",
            &[PluginCommandTarget::Library(library)]
        ),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
    assert!(matches!(
        store.plugin_operation_result(&current, &subject, completed.operation_id),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
    assert!(matches!(
        store.accept_plugin_command(&authority, &request(&p, subject, "stale"), b""),
        Err(StoreError::Plugin(PluginError::StaleAuthority))
    ));
}

#[test]
fn global_grant_authorizes_downloader_site_library_and_subscribe_targets() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let s = site();
    store.insert_site(&s).unwrap();
    let p = registered(
        &store,
        false,
        &[PluginGrant {
            capability: "command:fixture".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        }],
    );
    let state = store.issue_plugin_state_authority(&p, None).unwrap();
    for target in [
        PluginCommandTarget::Global,
        PluginCommandTarget::Downloader(domain::DownloaderId::new()),
        PluginCommandTarget::Site(s.id),
        PluginCommandTarget::Library(domain::LibraryId::new()),
        PluginCommandTarget::Subscribe(domain::SubscribeId::new()),
    ] {
        assert!(
            store
                .authorize_plugin_command(&state, "fixture", std::slice::from_ref(&target))
                .is_ok(),
            "Global grant must authorize {target:?}"
        );
    }

    let dl1 = domain::DownloaderId::new();
    let dl2 = domain::DownloaderId::new();
    let narrow_meta = metadata("org.example.narrow", false);
    let np = store.register_plugin_version(&narrow_meta, 0).unwrap();
    let np = store
        .replace_plugin_grants(
            &np.plugin_id,
            &np.package_hash,
            np.revision,
            &[grant("downloader", dl1)],
        )
        .unwrap();
    let np = enable(&store, &np);
    let nstate = store.issue_plugin_state_authority(&np, None).unwrap();
    assert!(
        store
            .authorize_plugin_command(&nstate, "fixture", &[PluginCommandTarget::Downloader(dl1)])
            .is_ok()
    );
    assert!(matches!(
        store.authorize_plugin_command(&nstate, "fixture", &[PluginCommandTarget::Global]),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
    assert!(matches!(
        store.authorize_plugin_command(&nstate, "fixture", &[PluginCommandTarget::Downloader(dl2)]),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
}
