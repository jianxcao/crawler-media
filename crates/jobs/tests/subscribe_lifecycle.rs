use jobs::{JobIdentity, Queue};
#[test]
fn stale_incarnation_publication_cannot_overwrite_newer_delivered_activation() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let queue = Queue::open(temp.path().join("jobs.db")).unwrap();
    let id = domain::SubscribeId::new();
    let identity = JobIdentity::subscribe_search(id);
    let new = serde_json::json!({"subscribe_id":id.to_string(),"policy":"new"}).to_string();
    let old = serde_json::json!({"subscribe_id":id.to_string(),"policy":"old"}).to_string();
    queue
        .apply_subscribe_config(id, "new-generation", 2, 1, |q| {
            q.enqueue(identity, &new, 0)?;
            Ok(())
        })
        .unwrap();
    queue
        .apply_subscribe_config(id, "old-generation", 1, 1, |q| {
            q.delete_def(identity)?;
            q.enqueue(identity, &old, 0)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        queue.jobs_for_identity(identity, 10).unwrap()[0].payload,
        new,
        "old incarnation must not overwrite newer observed activation"
    );
}
#[test]
fn stale_delete_preserves_newer_epoch_and_closed_epoch_cannot_reopen_after_plain_delete() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let queue = Queue::open(temp.path().join("jobs.db")).unwrap();
    let id = domain::SubscribeId::new();
    let identity = JobIdentity::subscribe_search(id);
    let payload = serde_json::json!({"subscribe_id":id.to_string()}).to_string();
    queue
        .apply_subscribe_config(id, "new", 2, 1, |q| {
            q.enqueue(identity, &payload, 0)?;
            Ok(())
        })
        .unwrap();
    assert!(
        !queue.close_subscribe_epoch(id, "old", 1).unwrap(),
        "stale delete must be ignored atomically"
    );
    assert_eq!(queue.jobs_for_identity(identity, 10).unwrap().len(), 1);
    queue.close_subscribe_epoch(id, "new", 2).unwrap();
    queue.delete_def(identity).unwrap();
    assert!(
        !queue
            .apply_subscribe_config(id, "new", 2, 100, |q| {
                q.enqueue(identity, &payload, 0)?;
                Ok(())
            })
            .unwrap(),
        "closed epoch cannot reopen even after normal identity cleanup"
    );
    assert!(queue.jobs_for_identity(identity, 10).unwrap().is_empty());
    queue.close_subscribe_epoch(id, "aborted", 3).unwrap();
    assert!(
        !queue
            .apply_subscribe_config(id, "aborted", 3, 1, |q| {
                q.enqueue(identity, &payload, 0)?;
                Ok(())
            })
            .unwrap()
    );
    assert!(
        queue
            .apply_subscribe_config(id, "future", 4, 1, |q| {
                q.enqueue(identity, &payload, 0)?;
                Ok(())
            })
            .unwrap()
    );
}
