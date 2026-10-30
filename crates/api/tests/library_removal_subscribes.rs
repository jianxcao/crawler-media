//! Task 6: actual HTTP removal and completed Transfer use the same-kind default.
#[path = "library_removal_subscribes/completion.rs"]
mod completion;
#[path = "library_removal_subscribes/support.rs"]
mod support;
use axum::http::StatusCode;
use domain::MediaKind;
use support::*;
use tower::ServiceExt;

#[test]
fn subscribe_bound_to_deleted_library_falls_back_to_default() {
    for kind in [MediaKind::Movie, MediaKind::Tv] {
        let fixture = Fixture::new(kind);
        fixture.pending(kind);
        runtime().block_on(async {
            let lease = fixture
                .state
                .acquire_library_work(&fixture.removed.id)
                .unwrap();
            let response = fixture
                .app()
                .oneshot(request(
                    "DELETE",
                    &format!("/api/v1/libraries/{}", fixture.removed.id),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            assert!(
                lease.control().is_cancelled(),
                "DELETE must immediately cancel Library work"
            );
            let body = json(response).await;
            let operation = body["data"]["operation_id"].as_str().unwrap();
            removed(&fixture, operation).await;
            assert_eq!(
                fixture.raw_binding(),
                None,
                "durable Subscribe binding must be NULL before success is exposed"
            );
            assert_eq!(fixture.binding(), None);
            assert!(lease.publish(|| Ok(())).is_err());
            let store = fixture.state.store();
            assert_eq!(
                store.lock().default_library(kind).unwrap().unwrap().id,
                fixture.default.id
            );
            let state = fixture.state.clone();
            let id = fixture.subscribe;
            let result = tokio::task::spawn_blocking(move || api::worker::transfer_one(&state, id))
                .await
                .unwrap();
            assert!(
                result.is_ok(),
                "Transfer must resolve the same-kind default: {result:?}"
            );
            let rows = store.lock().file_coverage_for_media(fixture.media).unwrap();
            assert_eq!(
                rows.len(),
                1,
                "a completed download must really publish into the default Library"
            );
            assert_eq!(rows[0].file.library_id.to_string(), fixture.default.id);
            assert!(
                rows[0]
                    .file
                    .path
                    .starts_with(&fixture.default.root_paths[0])
            );
            assert_eq!(
                std::fs::read(&rows[0].file.path).unwrap(),
                std::fs::read(&fixture.source).unwrap()
            );
            assert!(
                store
                    .lock()
                    .load_pending(fixture.subscribe)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                store
                    .lock()
                    .load_pending_state(fixture.subscribe, "imported")
                    .unwrap()
                    .len(),
                1
            );
        });
    }
}

#[test]
fn startup_recovery_detaches_bindings_after_a_succeeded_library_commit() {
    let fixture = Fixture::new(MediaKind::Movie);
    let receipt = fixture.succeeded_with_binding_gap();
    runtime().block_on(async {
        api::library_lifecycle::resume_library_removals(&fixture.state).unwrap();
        detached(&fixture).await;
        api::library_lifecycle::resume_library_removals(&fixture.state).unwrap();
        let store = fixture.state.store();
        assert_eq!(
            store
                .lock()
                .library_removal(&receipt.operation_id)
                .unwrap()
                .unwrap(),
            receipt
        );
        assert_eq!(fixture.binding(), None);
    });
}

#[test]
fn repeating_successful_removal_retries_detachment_instead_of_early_returning() {
    let fixture = Fixture::new(MediaKind::Tv);
    let receipt = fixture.succeeded_with_binding_gap();
    runtime().block_on(async {
        let retried =
            api::library_lifecycle::start_library_removal(&fixture.state, &fixture.removed.id)
                .unwrap();
        assert_eq!(retried.operation_id, receipt.operation_id);
        detached(&fixture).await;
        let again =
            api::library_lifecycle::start_library_removal(&fixture.state, &fixture.removed.id)
                .unwrap();
        assert_eq!(again.operation_id, receipt.operation_id);
        assert_eq!(fixture.binding(), None);
    });
}
