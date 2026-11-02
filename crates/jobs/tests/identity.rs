use domain::{MediaId, SubscribeId};
use jobs::{JobIdentity, JobKind, JobScope, NewDef, Queue, Schedule};

fn def(kind: JobKind, scope: JobScope, payload: &str) -> NewDef {
    NewDef {
        kind,
        scope,
        name: "identity".into(),
        enabled: true,
        schedule: Some(Schedule::Interval { secs: 600 }),
        payload: payload.into(),
        timeout_secs: None,
        concurrency_key: None,
    }
}

#[test]
fn global_identity_mutations_do_not_cross_equal_payloads() {
    let tmp = tempfile::tempdir().unwrap();
    let q = Queue::open(tmp.path().join("jobs.db")).unwrap();
    let transfer = q
        .create_def(def(JobKind::Transfer, JobScope::Global, "{}"))
        .unwrap();
    let scrape = q
        .create_def(def(JobKind::Scrape, JobScope::Global, "{}"))
        .unwrap();
    let identity = JobIdentity::global(JobKind::Transfer);
    q.set_def_enabled(identity, false).unwrap();
    assert!(!q.def(transfer.id).unwrap().unwrap().enabled);
    assert!(q.def(scrape.id).unwrap().unwrap().enabled);
    q.delete_def(identity).unwrap();
    assert!(q.def(scrape.id).unwrap().is_some());
}

#[test]
fn scoped_identity_survives_arguments_and_includes_defless_activity() {
    let tmp = tempfile::tempdir().unwrap();
    let q = Queue::open(tmp.path().join("jobs.db")).unwrap();
    let id = SubscribeId::new();
    let identity = JobIdentity::subscribe_search(id);
    let payload = format!("{{ \"extra\":true, \"subscribe_id\":\"{id}\" }}");
    let existing = q
        .ensure_def(def(identity.kind, identity.scope, &payload))
        .unwrap();
    q.set_def_enabled(identity, false).unwrap();
    let duplicate = q
        .ensure_def(def(identity.kind, identity.scope, &payload))
        .unwrap();
    assert_eq!(existing.id, duplicate.id);
    assert!(!duplicate.enabled);
    assert!(
        q.create_def(def(identity.kind, identity.scope, &payload))
            .is_err()
    );
    let job = q.enqueue(identity, &payload, 0).unwrap();
    assert!(job.def_id.is_none());
    assert_eq!(q.jobs_for_identity(identity, 10).unwrap()[0].id, job.id);
    let wrong = format!("{{\"subscribe_id\":\"{}\"}}", SubscribeId::new());
    assert!(q.enqueue(identity, &wrong, 0).is_err());
    assert!(
        q.update_def_config_cas(existing.id, &payload, &wrong, None)
            .is_err()
    );
    assert!(
        q.create_def(def(
            JobKind::Transfer,
            JobScope::Media(MediaId::new()),
            "{}"
        ))
        .is_err()
    );
}

#[test]
fn merge_coalesces_catalog_definitions_and_preserves_running_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("jobs.db");
    let q = Queue::open(&path).unwrap();
    let from = MediaId::new();
    let to = MediaId::new();
    let old = JobIdentity::catalog_refresh(from);
    let target = JobIdentity::catalog_refresh(to);
    let source = q
        .create_def(def(
            old.kind,
            old.scope,
            &format!("{{\"media_id\":\"{from}\",\"keep\":true}}"),
        ))
        .unwrap();
    let survivor = q
        .create_def(def(
            target.kind,
            target.scope,
            &format!("{{\"media_id\":\"{to}\"}}"),
        ))
        .unwrap();
    let running = q.ensure_scheduled_for(0, source.id).unwrap().remove(0);
    let claimed = q.claim(0, 1).unwrap().remove(0);
    q.set_job_progress(running.id, "receipt").unwrap();
    q.ensure_scheduled_for(0, source.id).unwrap();
    q.ensure_scheduled_for(0, survivor.id).unwrap();
    assert!(
        q.rewrite_media_refs(&from.to_string(), &to.to_string())
            .is_err()
    );
    assert_eq!(
        q.get(running.id).unwrap().unwrap().status,
        jobs::JobStatus::Running
    );
    q.succeed_claimed(&claimed, 1).unwrap();
    assert!(
        q.rewrite_media_refs(&from.to_string(), &to.to_string())
            .unwrap()
            > 0
    );
    assert!(q.def(source.id).unwrap().is_none());
    assert_eq!(q.def(survivor.id).unwrap().unwrap(), survivor);
    let receipt = q.get(running.id).unwrap().unwrap();
    assert_eq!(receipt.scope, old.scope);
    assert_eq!(receipt.progress.as_deref(), Some("receipt"));
    assert!(q.get_live_child(survivor.id).unwrap().is_some());
    drop(q);
    for _ in 0..2 {
        let q = Queue::open(&path).unwrap();
        assert_eq!(
            q.rewrite_media_refs(&from.to_string(), &to.to_string())
                .unwrap(),
            0
        );
        assert!(q.def_for_identity(old).unwrap().is_none());
        assert!(q.get_live_child(survivor.id).unwrap().is_some());
    }
}

#[test]
fn concurrent_ensure_uses_one_identity_and_preserves_existing_config() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("jobs.db");
    let q = Queue::open(&path).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let threads = (0..8)
        .map(|n| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let queue = Queue::open(path).unwrap();
                barrier.wait();
                let mut new = def(JobKind::Transfer, JobScope::Global, "{}");
                new.concurrency_key = Some(format!("key-{n}"));
                queue.ensure_def(new).unwrap().id
            })
        })
        .collect::<Vec<_>>();
    let ids = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect::<Vec<_>>();
    assert!(ids.iter().all(|id| *id == ids[0]));
    q.set_def_enabled(JobIdentity::global(JobKind::Transfer), false)
        .unwrap();
    let actual = q
        .ensure_def(def(JobKind::Transfer, JobScope::Global, "new arguments"))
        .unwrap();
    assert!(!actual.enabled);
    assert_eq!(actual.payload, "{}");
}

#[test]
fn identity_mutations_share_subscribe_receipt_atomicity_and_cleanup_ownership() {
    let tmp = tempfile::tempdir().unwrap();
    let q = Queue::open(tmp.path().join("jobs.db")).unwrap();
    let id = SubscribeId::new();
    let identity = JobIdentity::subscribe_search(id);
    let payload = format!("{{\"subscribe_id\":\"{id}\"}}");
    let work = |q: &Queue| {
        q.ensure_def(def(identity.kind, identity.scope, &payload))?;
        q.set_def_enabled(identity, false)?;
        Ok(())
    };
    assert!(
        q.apply_subscribe_config(id, "generation", 1, |q| {
            work(q)?;
            Err(jobs::JobError::Identity("rollback".into()))
        })
        .is_err()
    );
    assert!(q.def_for_identity(identity).unwrap().is_none());
    assert!(q.apply_subscribe_config(id, "generation", 1, work).unwrap());
    q.set_def_enabled(identity, true).unwrap();
    assert!(
        !q.apply_subscribe_config(id, "generation", 1, |_| panic!("receipt replay"))
            .unwrap()
    );
    let orphan = q.enqueue(identity, &payload, 0).unwrap();
    q.delete_def(identity).unwrap();
    assert!(q.get(orphan.id).unwrap().is_none());
    assert!(
        q.apply_subscribe_config(id, "generation", 1, work).unwrap(),
        "owned receipt removed with identity"
    );
}

#[test]
fn disabling_definition_is_atomic_if_queued_cancellation_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let q = Queue::open(tmp.path().join("jobs.db")).unwrap();
    let identity = JobIdentity::global(JobKind::Transfer);
    let definition = q
        .create_def(def(identity.kind, identity.scope, "{}"))
        .unwrap();
    q.ensure_scheduled_for(0, definition.id).unwrap();
    q.with_connection(|c|c.execute_batch("CREATE TRIGGER reject_cancel BEFORE UPDATE ON jobs BEGIN SELECT RAISE(FAIL,'cancel rejected'); END;")).unwrap();
    assert!(q.set_def_enabled_by_id(definition.id, false).is_err());
    assert!(
        q.def(definition.id).unwrap().unwrap().enabled,
        "failed cancellation must roll back toggle"
    );
}
