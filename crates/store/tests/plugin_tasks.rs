#[path = "plugin_support/mod.rs"]
mod plugin_support;
use plugin_support::*;
use store::{Store, StoreError, plugins::*};

#[test]
fn task_projection_intents_are_bounded_stable_and_acknowledge_only_exact_generation_revision() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p = enable(&store, &install(&store, "org.example.tasks", &[]));
    let definition = PluginTaskDefinition {
        enabled: true,
        interval_secs: Some(300),
        public_config: serde_json::json!({"slot_ref":"fixture"}),
    };
    for i in 0..205 {
        store
            .save_plugin_task_binding(&p, &format!("task-{i:03}"), 0, &definition)
            .unwrap();
    }
    assert_eq!(
        store.pending_plugin_task_sync(None, None).unwrap().len(),
        100
    );
    let first = store.pending_plugin_task_sync(None, Some(999)).unwrap();
    assert_eq!(first.len(), 200);
    let rest = store
        .pending_plugin_task_sync(Some(first.last().unwrap().binding_id), Some(200))
        .unwrap();
    assert_eq!(rest.len(), 5);
    let original = store
        .get_plugin_task_binding(&p.plugin_id, "task-000")
        .unwrap()
        .unwrap();
    assert!(original.projection_pending());
    assert!(matches!(
        store.acknowledge_plugin_task_sync(
            original.binding_id,
            original.generation + 1,
            original.definition_revision,
            Some("job-fixture")
        ),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    let changed = store
        .save_plugin_task_binding(
            &p,
            "task-000",
            original.definition_revision,
            &PluginTaskDefinition {
                enabled: false,
                ..definition
            },
        )
        .unwrap();
    assert_eq!(changed.binding_id, original.binding_id);
    assert!(
        store
            .acknowledge_plugin_task_sync(
                original.binding_id,
                original.generation,
                original.definition_revision,
                None
            )
            .is_err()
    );
    store
        .acknowledge_plugin_task_sync(
            changed.binding_id,
            changed.generation,
            changed.definition_revision,
            Some("job-fixture"),
        )
        .unwrap();
    let projected = Store::open(dir.path())
        .unwrap()
        .get_plugin_task_binding(&p.plugin_id, "task-000")
        .unwrap()
        .unwrap();
    assert!(!projected.projection_pending());
    assert_eq!(projected.job_def_id.as_deref(), Some("job-fixture"));
    let draining = store
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), false)
        .unwrap();
    let disabled = store
        .get_plugin_task_binding(&p.plugin_id, "task-000")
        .unwrap()
        .unwrap();
    assert!(!disabled.definition.enabled);
    assert_eq!(disabled.generation, draining.generation);
    assert!(disabled.projection_pending());
    assert!(
        store
            .acknowledge_plugin_task_sync(
                projected.binding_id,
                projected.generation,
                projected.definition_revision,
                None
            )
            .is_err()
    );
    store
        .acknowledge_plugin_task_sync(
            disabled.binding_id,
            disabled.generation,
            disabled.definition_revision,
            None,
        )
        .unwrap();
}

#[test]
fn app_transaction_rolls_back_operation_and_task_intent_together() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p = install(&store, "org.example.tasks", &[]);
    let input = request(&p, PluginSubject::User(domain::UserId::new()), "atomic");
    let definition = PluginTaskDefinition {
        enabled: false,
        interval_secs: None,
        public_config: serde_json::json!({}),
    };
    let result = store.app_write(|app| {
        app.accept_plugin_operation(&input)?;
        app.save_plugin_task_binding(&p, "task", 0, &definition)?;
        Err::<(), _>(StoreError::Invalid("abort fixture".into()))
    });
    assert!(result.is_err());
    assert!(
        store
            .pending_plugin_commands(None, None, 100)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .pending_plugin_task_sync(None, None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.get_plugin_task_binding(&p.plugin_id, "task").unwrap(),
        None
    );
    assert!(!store.accept_plugin_operation(&input).unwrap().replayed);
}
