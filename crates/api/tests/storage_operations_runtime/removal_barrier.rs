//! Task 13 Step 3, first item: `begin` must not freeze a Library a removal already owns.
//!
//! The HTTP surface refuses such a PATCH before any operation exists, so the user-visible
//! consequence is a `400 library.protected` and an unchanged revision — no second owner.

use super::*;

#[tokio::test]
async fn patch_is_refused_while_a_removal_owns_the_library() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let seeded = seed_library(&state, &temp);
    let revision_before = {
        let store = state.store();
        let store = store.lock();
        // A removal needs a second Library of the same kind to exist first.
        store
            .create_library(MediaKind::Movie, "Second", &["/s/t"], "everyone", true, &[])
            .unwrap();
        let removal = store.begin_library_removal(&seeded.library_id).unwrap();
        assert_eq!(removal.state, "frozen");
        assert_eq!(
            store.library_lifecycle_state(&seeded.library_id).unwrap(),
            "ready",
            "a removal freezes its own ledger, not the Library lifecycle"
        );
        store.library_config_revision(&seeded.library_id).unwrap()
    };

    let app = router(state.clone());
    let target = new_root(&temp);
    let response = app
        .oneshot(json_request(
            "PATCH",
            &format!("/api/v1/libraries/{}", seeded.library_id),
            serde_json::json!({ "root_paths": [target.to_str().unwrap()] }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "library.protected", "{body}");

    let store = state.store();
    let store = store.lock();
    assert!(
        store.unfinished_library_operations().unwrap().is_empty(),
        "the refusal must not leave a resumable reconfiguration behind"
    );
    assert_eq!(
        store.library_config_revision(&seeded.library_id).unwrap(),
        revision_before,
        "the refusal must not consume a revision"
    );
    assert_eq!(
        store.library_lifecycle_state(&seeded.library_id).unwrap(),
        "ready"
    );
}
