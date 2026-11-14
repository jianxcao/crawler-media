//! L6: subscription paths delete replaced files through the M6 queue (file IO
//! outside the Store mutex) and pending cleanup holds the Subscribe guard.
//! Source audit catches in-lock deletion; a real Router request checks cleanup
//! against a concurrent Subscribe owner.

use super::harness::Harness;

fn another_owned_task() -> super::mock_qbit::Task {
    super::mock_qbit::Task {
        hash: "f".repeat(40),
        name: "Other.Show.S01E01".into(),
        tags: vec![
            "crawler-media".into(),
            downloader::ownership_tag("https://pt.example/other"),
        ],
        progress: 0.5,
        save_path: "/downloads".into(),
        files: vec![],
        size: 1,
        state: "downloading".into(),
        source: "https://pt.example/other".into(),
    }
}

fn source(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn subscription_paths_do_not_delete_files_under_the_store_lock() {
    for rel in [
        "src/worker/transfer.rs",
        "src/management/subscribes/run.rs",
        "src/http/downloaders/task_listing.rs",
    ] {
        let text = source(rel);
        assert!(
            !text.contains("remove_file_and_ledger"),
            "{rel} must enqueue deletions (M6) instead of deleting under the Store lock"
        );
    }
    let queue = source("src/http/file_delete.rs");
    let body = queue
        .split("fn process_queued_deletion")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("process_queued_deletion");
    let delete_at = body.find("remove_physical_file").expect("deletes files");
    assert!(
        !body[..delete_at].contains("= store.lock()"),
        "the Store lock must not be held while removing the file"
    );
}

#[tokio::test]
async fn pending_cleanup_holds_the_subscribe_guard() {
    let harness = Harness::new();
    harness.create_site().await;
    let id = harness.create_movie_subscribe(serde_json::json!({})).await;
    harness.run_subscribe(id).await;
    assert_eq!(harness.pending_state(id, "active"), 1);
    harness.qb.remove_all();
    // A nonempty, owned snapshot proves the client supports ownership tags;
    // an empty client list alone must never release an HTTP pending row.
    harness.qb.inject(another_owned_task());
    harness.age_pending(id, 16 * 60);

    let subscribe = harness.state.subscribe_guard(id);
    let (held, ready) = std::sync::mpsc::channel();
    let (release, finished) = std::sync::mpsc::channel();
    // The holder's wall-clock watchdog can release the guard even if a future
    // regression synchronously parks this test's only Tokio runtime thread.
    let holder = std::thread::spawn(move || {
        let _held = subscribe
            .try_lock_for(std::time::Duration::from_secs(3))
            .expect("fixture Subscribe must be idle before the competing owner starts");
        let _ = held.send(());
        finished
            .recv_timeout(std::time::Duration::from_secs(3))
            .is_ok()
    });
    ready
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    let listed =
        tokio::time::timeout(std::time::Duration::from_secs(3), harness.list_tasks()).await;
    let _ = release.send(());
    let held_until_released = holder.join().unwrap();
    assert!(
        held_until_released,
        "listing waited for the independent guard watchdog instead of skipping its owner"
    );
    let listed =
        listed.expect("listing must skip the busy Subscribe rather than wait for its owner");
    let items = listed["data"]["items"]
        .as_array()
        .expect("successful task listing");
    assert_eq!(
        items.len(),
        1,
        "the concurrent owner keeps its pending task visible: {listed}"
    );
    assert_eq!(items[0]["state"], "missing");
    assert_eq!(
        harness.pending_state(id, "active"),
        1,
        "cleanup must preserve the active owner's pending row"
    );

    let listed = harness.list_tasks().await;
    assert!(
        listed["data"]["items"].as_array().unwrap().is_empty(),
        "the same stale task can be cleaned after releasing the Subscribe: {listed}"
    );
    assert_eq!(harness.pending_state(id, "active"), 0);
}
