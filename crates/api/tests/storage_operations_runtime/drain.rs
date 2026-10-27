//! Task 13 Step 3, second item: the `lease` drain.
//!
//! `begin` freezes the Library in the Store, but a worker that already holds a lease
//! must not be swept into the reconfiguration window: `block` cancels it and the driver
//! waits for the registry to go idle before the first batch.

use std::time::Duration;

use super::*;

async fn operation_state(app: &axum::Router, library_id: &str, operation_id: &str) -> String {
    let path = format!("/api/v1/libraries/{library_id}/operations/{operation_id}");
    let response = app.clone().oneshot(request("GET", &path)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await["data"]["state"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// A fresh process has no work at all, and the flag tracks a real lease both ways.
#[tokio::test]
async fn has_active_work_is_false_until_a_lease_is_taken() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let seeded = seed_library(&state, &temp);

    assert!(
        !state.library_work().has_active_work(),
        "a fresh state must have no active Library work"
    );
    let lease = state.acquire_library_work(&seeded.library_id).unwrap();
    assert!(
        state.library_work().has_active_work(),
        "a held lease is active work"
    );
    drop(lease);
    assert!(
        !state.library_work().has_active_work(),
        "releasing the lease must drain the entry"
    );
}

/// The in-process case: a lease held when the `202` arrives is cancelled, and the
/// batches wait for it, so no worker stays inside the reconfiguration window.
#[tokio::test]
async fn an_in_flight_lease_is_cancelled_and_drained_before_the_first_batch() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let seeded = seed_library(&state, &temp);
    let target = new_root(&temp);
    let app = router(state.clone());

    let lease = state.acquire_library_work(&seeded.library_id).unwrap();
    assert!(!lease.control().is_cancelled(), "the lease starts healthy");
    let response = app
        .clone()
        .oneshot(json_request(
            "PATCH",
            &format!("/api/v1/libraries/{}", seeded.library_id),
            serde_json::json!({ "root_paths": [target.to_str().unwrap()] }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let operation_id = body_json(response).await["data"]["operation_id"]
        .as_str()
        .unwrap()
        .to_string();

    assert!(
        lease.control().is_cancelled(),
        "an in-flight lease must be cancelled, never swept into the window"
    );
    assert!(
        lease.publish(|| Ok(())).is_err(),
        "a cancelled lease must refuse to publish into the frozen Library"
    );

    // The drain has teeth: while the lease is held the driver cannot finish a batch, so
    // the operation stays in its persisted `frozen` phase instead of racing the worker.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        operation_state(&app, &seeded.library_id, &operation_id).await,
        "frozen",
        "no batch may be applied before the Library work is drained"
    );
    let old_root_id = root_id_of(&state.store().lock(), &seeded.library_id, &seeded.old_root);
    let conn = rusqlite::Connection::open(temp.path().join("data").join("library.db")).unwrap();
    let owner: String = conn
        .query_row(
            "SELECT root_id FROM library_files WHERE file_id=?1",
            rusqlite::params![seeded.file_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        owner, old_root_id,
        "the file must not move while the lease still holds the Library"
    );

    drop(lease);
    let done = poll_operation(&app, &seeded.library_id, &operation_id).await;
    assert_eq!(done["data"]["state"], "succeeded");
    assert!(
        !state.library_work().has_active_work(),
        "the drained Library is idle again"
    );
}
