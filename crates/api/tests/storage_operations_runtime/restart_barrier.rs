//! Task 13 restart evidence: the reconfiguration freeze is committed Store state, not
//! process memory, so a restarted process still refuses the old config-worker publish with
//! `409 library.reindexing` until startup recovery drives the operation to `succeeded`.

use super::*;

/// First process: seed a Library, begin a root reconfiguration and report the freeze.
///
/// `state` — and with it the Store connection and the in-memory work registry — goes away
/// when this returns, so everything asserted afterwards has to come from committed state.
fn freeze_a_library_then_exit(temp: &tempfile::TempDir) -> Seeded {
    let state = stage(temp);
    let seeded = seed_library(&state, temp);
    let target = new_root(temp);
    let handle = state.store();
    let store = handle.as_ref();
    let revision = store.library_config_revision(&seeded.library_id).unwrap();
    let status = store
        .begin_library_reconfiguration(
            &store::library_operations::LibraryReconfiguration {
                operation_id: "op-restart".into(),
                library_id: seeded.library_id.parse().unwrap(),
                expected_revision: revision,
                root_paths: Some(vec![target]),
                display_locale: None,
            },
            1_000,
        )
        .unwrap();
    assert_eq!(status.state, "frozen");
    seeded
}

/// Second process: the freeze must be visible with nothing held in memory.
fn restarted_frozen_facts(
    state: &ApiState,
    library_id: &str,
) -> store::library_operations::LibraryOperationView {
    assert!(
        !state.library_work().has_active_work(),
        "nothing is frozen in memory, only in the Store"
    );
    let handle = state.store();
    let store = handle.as_ref();
    assert_eq!(
        store.library_lifecycle_state(library_id).unwrap(),
        "reindexing",
        "the committed freeze still bars writes"
    );
    assert_eq!(
        store.unfinished_library_operations().unwrap(),
        vec!["op-restart".to_string()],
        "the operation stays visible for startup recovery"
    );
    store
        .library_operation_view("op-restart")
        .unwrap()
        .expect("the operation row outlives the process that created it")
}

/// The persisted barrier alone must refuse the old config-worker publish with 409.
async fn assert_persisted_barrier_refuses_config_write(app: &axum::Router, library_id: &str) {
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{library_id}/operations/op-restart"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["data"]["state"], "frozen");

    let response = app
        .clone()
        .oneshot(json_request(
            "PATCH",
            &format!("/api/v1/libraries/{library_id}"),
            serde_json::json!({ "name": "Renamed" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "the old config-worker publish is refused by persisted state alone"
    );
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "library.reindexing");
    assert_eq!(body["error"]["operation_id"], "op-restart");
}

/// A resumed success releases the freeze and finishes the migration it had started.
fn assert_resumed_success_releases_the_freeze(
    state: &ApiState,
    temp: &tempfile::TempDir,
    seeded: &Seeded,
) {
    let handle = state.store();
    let store = handle.as_ref();
    assert_eq!(
        store.library_lifecycle_state(&seeded.library_id).unwrap(),
        "ready",
        "a succeeded reconfiguration must release the freeze"
    );
    let target = new_root(temp);
    let page = store
        .library_file_page(
            seeded.library_id.parse().unwrap(),
            seeded.media_id,
            None,
            10,
        )
        .unwrap();
    assert_eq!(
        page.files[0].root_id,
        root_id_of(store, &seeded.library_id, &target),
        "resumed recovery must finish the migration"
    );
}

/// The freeze is committed state, not process memory: a restarted process (new Store
/// connection, empty in-memory work registry) still reports the operation as `frozen`,
/// still refuses the old config-worker publish with `409 library.reindexing`, and startup
/// recovery can still drive it to `succeeded`, after which writes are accepted again.
#[tokio::test]
async fn freeze_barrier_survives_a_process_restart() {
    let temp = tempfile::tempdir().unwrap();
    let seeded = freeze_a_library_then_exit(&temp);

    let restarted = stage(&temp);
    let frozen = restarted_frozen_facts(&restarted, &seeded.library_id);
    assert_eq!(frozen.state, "frozen");
    assert_eq!(frozen.kind, "roots");

    let app = router(restarted.clone());
    assert_persisted_barrier_refuses_config_write(&app, &seeded.library_id).await;

    api::storage_operations::resume_library_reconfigurations(&restarted).unwrap();
    let status = poll_operation(&app, &seeded.library_id, "op-restart").await;
    assert_eq!(status["data"]["state"], "succeeded");
    assert_resumed_success_releases_the_freeze(&restarted, &temp, &seeded);

    let response = app
        .oneshot(json_request(
            "PATCH",
            &format!("/api/v1/libraries/{}", seeded.library_id),
            serde_json::json!({ "name": "Renamed" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "after success the Library accepts config writes again"
    );
    assert_eq!(body_json(response).await["data"]["name"], "Renamed");
}
