use super::support::*;
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::sync::Arc;

#[test]
fn transfer_releases_subscribe_guard_during_downloader_io() {
    let gate = Arc::new(Gate::default());
    let source = tempfile::tempdir_in("/tmp").unwrap();
    let path = source.path().join("The.Matrix.1999.1080p.WEB-DL.mkv");
    std::fs::write(&path, b"finite completed transfer fixture").unwrap();
    let fixture = Fixture::new(Arc::new(GatedDownloader {
        gate: gate.clone(),
        completed: path,
    }));
    fixture.pending();
    let _release = ReleaseOnDrop(gate.clone());
    let runtime = runtime();
    runtime.block_on(async {
        let state = fixture.state.clone();
        let id = fixture.subscribe;
        let transfer = tokio::task::spawn_blocking(move || api::worker::transfer_one(&state, id));
        gate.entered().await;
        let uri = format!("/api/v1/subscriptions/{id}");
        let detail = tokio::time::timeout(SLO, fixture.request("GET", &uri, Value::Null)).await;
        // PATCH actually needs subscribe_guard; GET alone already skips it and
        // cannot prove that Transfer releases the guard during Downloader IO.
        let mutation = tokio::time::timeout(
            SLO,
            fixture.request("PATCH", &uri, json!({"tracking_state": "paused"})),
        )
        .await;
        gate.release();
        let _outcome = transfer.await.unwrap();
        let (status, body) =
            detail.expect("same Subscribe detail must complete during blocked Downloader IO");
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = mutation
            .expect("same Subscribe mutation must complete while Downloader IO remains blocked");
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["tracking_state"], "paused");
        // The IO snapshot became stale while paused. A late worker cannot
        // overwrite the choice or publish the already-observed completed file.
        let handle = fixture.state.store();
        let store = handle.as_ref();
        assert_eq!(
            store.get_subscribe(id).unwrap().unwrap().tracking_state,
            "paused"
        );
        assert_eq!(store.load_pending(id).unwrap().len(), 1);
        assert!(
            store
                .file_coverage_for_media(fixture.media)
                .unwrap()
                .is_empty()
        );
    });
}

#[test]
fn duplicate_transfer_rounds_preserve_the_active_round_until_delete_cleanup() {
    let gate = Arc::new(Gate::default());
    let source = tempfile::tempdir_in("/tmp").unwrap();
    let path = source.path().join("The.Matrix.1999.1080p.WEB-DL.mkv");
    std::fs::write(&path, b"finite completed transfer fixture").unwrap();
    let fixture = Fixture::new(Arc::new(GatedDownloader {
        gate: gate.clone(),
        completed: path.clone(),
    }));
    fixture.pending();
    let _release = ReleaseOnDrop(gate.clone());
    let runtime = runtime();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                let id = fixture.subscribe;
                let state = fixture.state.clone();
                let first =
                    tokio::task::spawn_blocking(move || api::worker::transfer_one(&state, id));
                gate.entered().await;
                // Both rejected rounds must leave the first round's ownership intact.
                // Losing that marker lets the third attempt enter the blocked IO path.
                let state = fixture.state.clone();
                let mut duplicates = tokio::task::spawn_blocking(move || {
                    let second = api::worker::transfer_one(&state, id);
                    let third = api::worker::transfer_one(&state, id);
                    (second, third)
                });
                let duplicate_result = tokio::time::timeout(SLO, &mut duplicates).await.ok();
                let duplicates_returned = duplicate_result.is_some();
                let uri = format!("/api/v1/subscriptions/{id}?delete_library_files=true");
                let mut deletion = Box::pin(fixture.request("DELETE", &uri, Value::Null));
                let early_delete = tokio::time::timeout(SLO, &mut deletion).await.ok();
                let deletion_waited = early_delete.is_none();
                gate.release();
                let first_result = first.await.unwrap();
                let (second, third) = match duplicate_result {
                    Some(result) => result.unwrap(),
                    None => duplicates.await.unwrap(),
                };
                let (status, body) = match early_delete {
                    Some(result) => result,
                    None => deletion.await,
                };
                assert!(
                    duplicates_returned,
                    "duplicate rounds must finish while the admitted round remains blocked"
                );
                assert!(
                    deletion_waited,
                    "deletion must retain the admitted round's effects"
                );
                assert!(first_result.is_ok(), "{first_result:?}");
                assert!(second.is_ok(), "{second:?}");
                assert!(third.is_ok(), "{third:?}");
                assert_eq!(status, StatusCode::OK, "{body}");
                let store = fixture.state.store();
                assert!(store.get_subscribe(id).unwrap().is_none());
                assert!(store.load_pending(id).unwrap().is_empty());
                assert!(
                    store
                        .file_coverage_for_media(fixture.media)
                        .unwrap()
                        .is_empty()
                );
                assert!(
                    store
                        .load_subscribe_facts(id)
                        .unwrap()
                        .entries()
                        .next()
                        .is_none()
                );
                assert_eq!(
                    std::fs::read(path).unwrap(),
                    b"finite completed transfer fixture"
                );
            })
            .await
        })
    }));
    // A broken ownership marker may deadlock a blocking worker on its own
    // mutex. Always release IO and bound runtime shutdown before reporting red.
    gate.release();
    runtime.shutdown_timeout(std::time::Duration::from_millis(100));
    let outcome = outcome.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    assert!(
        outcome.is_ok(),
        "duplicate round ownership must finish within the watchdog"
    );
}
