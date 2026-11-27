use std::sync::{Arc, Barrier};
use store::{Store, StoreError, plugins::*};
#[path = "plugin_support/mod.rs"]
mod plugin_support;

fn request(subject: PluginSubject) -> AcceptPluginOperation {
    AcceptPluginOperation {
        plugin_id: None,
        subject,
        idempotency_key: "stable-key".into(),
        request: CanonicalPluginRequest {
            method: "POST".into(),
            path: "/plugins/install".into(),
            body_digest: [3; 32],
        },
        kind: "install".into(),
        expected_revision: None,
        expected_generation: None,
        cause: PluginCommandCause::root("cause-stable-key"),
    }
}

#[test]
fn concurrent_null_plugin_idempotency_is_atomic_and_request_identity_is_complete() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let p = plugin_support::install(&store, "org.example.operations", &[]);
    for plugin_id in [None, Some(p.plugin_id.clone())] {
        let mut input = request(PluginSubject::User(domain::UserId::new()));
        if plugin_id.is_some() {
            input.expected_revision = Some(p.revision);
            input.expected_generation = Some(p.generation);
        }
        input.plugin_id = plugin_id;
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (store, barrier, input) = (store.clone(), barrier.clone(), input.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    store.accept_plugin_operation(&input).unwrap()
                })
            })
            .collect();
        let accepted: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let id = accepted[0].operation.operation_id;
        assert_eq!(accepted.iter().filter(|v| !v.replayed).count(), 1);
        assert!(accepted.iter().all(|v| v.operation.operation_id == id));
        for field in 0..3 {
            let mut changed = input.clone();
            match field {
                0 => changed.request.method = "PUT".into(),
                1 => changed.request.path = "/plugins/other".into(),
                _ => changed.request.body_digest = [9; 32],
            }
            assert!(matches!(
                store.accept_plugin_operation(&changed),
                Err(StoreError::Plugin(PluginError::Conflict))
            ));
        }
        assert_eq!(
            Store::open(dir.path())
                .unwrap()
                .accept_plugin_operation(&input)
                .unwrap()
                .operation
                .operation_id,
            id
        );
    }
}

#[test]
fn canonical_subjects_isolate_endpoint_keys_issuer_and_plugin_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let user = domain::UserId::new();
    let key = uuid::Uuid::new_v4();
    let subjects = [
        PluginSubject::User(user),
        PluginSubject::EndpointKey {
            key_id: key,
            issuer_user_id: user,
        },
        PluginSubject::EndpointKey {
            key_id: uuid::Uuid::new_v4(),
            issuer_user_id: user,
        },
        PluginSubject::EndpointKey {
            key_id: key,
            issuer_user_id: domain::UserId::new(),
        },
    ];
    for (index, subject) in subjects.iter().enumerate() {
        let accepted = store
            .accept_plugin_operation(&request(subject.clone()))
            .unwrap();
        assert_eq!(
            store
                .get_plugin_operation(subject, accepted.operation.operation_id)
                .unwrap(),
            Some(accepted.operation.clone())
        );
        for (other, subject) in subjects.iter().enumerate() {
            if other != index {
                assert_eq!(
                    store
                        .get_plugin_operation(subject, accepted.operation.operation_id)
                        .unwrap(),
                    None
                );
            }
        }
    }
    assert!(matches!(
        store.accept_plugin_operation(&request(PluginSubject::Task {
            plugin_id: "org.example.test".into(),
            task_id: "check".into()
        })),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
}

#[test]
fn cross_scope_idempotency_key_rejected_with_idempotency_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p = plugin_support::install(&store, "org.example.opcross", &[]);
    let user = domain::UserId::new();
    let mut first = request(PluginSubject::User(user));
    first.plugin_id = Some(p.plugin_id.clone());
    first.expected_revision = Some(p.revision);
    first.expected_generation = Some(p.generation);
    first.idempotency_key = "cross-scope-key".into();
    store.accept_plugin_operation(&first).unwrap();

    let mut second = request(PluginSubject::User(user));
    second.plugin_id = None;
    second.idempotency_key = "cross-scope-key".into();
    assert!(matches!(
        store.accept_plugin_operation(&second),
        Err(StoreError::Plugin(PluginError::IdempotencyConflict))
    ));
}
