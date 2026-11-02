use domain::MediaId;
use jobs::{JobIdentity, JobKind, JobScope, JobStatus, NewDef, Queue};
use serde_json::json;

#[test]
fn exact_media_identity_rewrites_definition_and_defless_pending_but_keeps_history() {
    let temp = tempfile::tempdir().unwrap();
    let queue = Queue::open(temp.path().join("jobs.db")).unwrap();
    let from = MediaId::new();
    let to = MediaId::new();
    let other = MediaId::new();
    let identity = JobIdentity::catalog_refresh(from);
    let def = queue
        .create_def(NewDef {
            kind: identity.kind,
            scope: identity.scope,
            name: "catalog".into(),
            enabled: true,
            schedule: None,
            payload: json!({"media_id":from,"extra":"keep"}).to_string(),
            timeout_secs: None,
            concurrency_key: Some(format!("catalog:{from}")),
        })
        .unwrap();
    let terminal = queue
        .enqueue(identity, &json!({"media_id":from}).to_string(), 0)
        .unwrap();
    queue.claim(0, 1).unwrap();
    queue.succeed(terminal.id, 1).unwrap();
    let queued = queue
        .enqueue(
            identity,
            &json!({"media_id":from,"extra":"keep"}).to_string(),
            0,
        )
        .unwrap();
    let unrelated = queue
        .enqueue(
            JobIdentity::catalog_refresh(other),
            &json!({"media_id":other,"nested":{"media_id":from},"note":from}).to_string(),
            0,
        )
        .unwrap();
    let global = queue
        .enqueue(
            JobIdentity::global(JobKind::Transfer),
            &json!({"media_id":from}).to_string(),
            0,
        )
        .unwrap();
    assert_eq!(
        queue
            .rewrite_media_refs(&from.to_string(), &to.to_string())
            .unwrap(),
        2
    );
    assert_eq!(
        queue
            .rewrite_media_refs(&from.to_string(), &to.to_string())
            .unwrap(),
        0
    );
    let updated = queue.def(def.id).unwrap().unwrap();
    assert_eq!(updated.scope, JobScope::Media(to));
    assert_eq!(updated.concurrency_key, Some(format!("catalog:{to}")));
    let pending = queue.get(queued.id).unwrap().unwrap();
    assert_eq!(pending.scope, JobScope::Media(to));
    assert!(pending.def_id.is_none());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&pending.payload).unwrap()["extra"],
        "keep"
    );
    assert_eq!(
        queue.get(terminal.id).unwrap().unwrap().scope,
        JobScope::Media(from)
    );
    assert_eq!(queue.get(unrelated.id).unwrap().unwrap(), unrelated);
    assert_eq!(queue.get(global.id).unwrap().unwrap(), global);
}

#[test]
fn corrupt_target_payload_rolls_back_merge_without_losing_pending_work() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("jobs.db");
    let queue = Queue::open(&path).unwrap();
    let from = MediaId::new();
    let to = MediaId::new();
    let job = queue
        .enqueue(
            JobIdentity::catalog_refresh(from),
            &json!({"media_id":from}).to_string(),
            0,
        )
        .unwrap();
    queue
        .with_connection(|conn| {
            conn.execute(
                "UPDATE jobs SET payload='invalid' WHERE id=?1",
                [job.id.to_string()],
            )
        })
        .unwrap();
    assert!(
        queue
            .rewrite_media_refs(&from.to_string(), &to.to_string())
            .is_err()
    );
    let preserved = queue.get(job.id).unwrap().unwrap();
    assert_eq!(preserved.scope, JobScope::Media(from));
    assert_eq!(preserved.status, JobStatus::Queued);
}
