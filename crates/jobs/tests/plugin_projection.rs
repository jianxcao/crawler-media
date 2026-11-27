use domain::{PluginOperationId, PluginTaskBindingId};
use jobs::{
    JobError, JobIdentity, JobKind, JobScope, JobStatus, NewDef, NewPluginOperationJob,
    PluginTaskSpec, Queue, QueueRead, Schedule,
};
use rusqlite::{Connection, OpenFlags};

fn open_reader(path: &std::path::Path) -> Connection {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.pragma_update(None, "query_only", true).unwrap();
    conn
}

fn task_spec(
    binding_id: PluginTaskBindingId,
    generation: i64,
    definition_revision: i64,
    enabled: bool,
    note: &str,
) -> PluginTaskSpec {
    PluginTaskSpec {
        binding_id,
        generation,
        definition_revision,
        name: format!("plugin-task-{binding_id}"),
        enabled,
        schedule: Some(Schedule::Interval { secs: 600 }),
        payload: serde_json::json!({ "binding_id": binding_id, "note": note }).to_string(),
        timeout_secs: Some(60),
        concurrency_key: Some(format!("plugin-task:{binding_id}")),
    }
}

fn op_job(
    operation_id: PluginOperationId,
    kind: JobKind,
    sequence: u32,
    generation: Option<i64>,
    request_key: &str,
) -> NewPluginOperationJob {
    NewPluginOperationJob {
        operation_id,
        kind,
        sequence,
        generation,
        request_key: request_key.into(),
        payload: serde_json::json!({ "operation_id": operation_id, "seq": sequence }).to_string(),
        run_after: 0,
        concurrency_key: None,
        timeout_secs: Some(30),
    }
}

#[test]
fn distinct_plugin_task_bindings_roundtrip_and_reject_mismatched_identities() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let path = tmp.path().join("jobs.db");
    let q = Queue::open(&path).unwrap();
    let (a, b) = (PluginTaskBindingId::new(), PluginTaskBindingId::new());
    let pa = q
        .project_plugin_task(&task_spec(a, 1, 1, true, "a"))
        .unwrap();
    let pb = q
        .project_plugin_task(&task_spec(b, 1, 1, true, "b"))
        .unwrap();
    assert!(pa.applied && pb.applied && pa.fence.def_id != pb.fence.def_id);
    q.ensure_scheduled(0).unwrap();
    assert_eq!(q.claim(0, 10).unwrap().len(), 2);
    drop(q);

    let q = Queue::open(&path).unwrap();
    let reader_conn = open_reader(&path);
    let reader = QueueRead::new(&reader_conn);
    assert_eq!(reader.list_defs().unwrap().len(), 2);
    assert_ne!(
        reader
            .def_for_identity(JobIdentity::plugin_task(a))
            .unwrap()
            .unwrap()
            .id,
        reader
            .def_for_identity(JobIdentity::plugin_task(b))
            .unwrap()
            .unwrap()
            .id,
    );
    assert_eq!(
        reader
            .jobs_for_identity(JobIdentity::plugin_task(a), 10)
            .unwrap()
            .len(),
        1
    );

    let op = PluginOperationId::new();
    for bad in [
        (JobKind::PluginTask, JobScope::Global, "{}".to_string()),
        (
            JobKind::PluginCommand,
            JobScope::PluginTask(a),
            format!("{{\"operation_id\":\"{op}\"}}"),
        ),
        (
            JobKind::Transfer,
            JobScope::PluginOperation(op),
            "{}".to_string(),
        ),
        (
            JobKind::PluginTask,
            JobScope::PluginTask(a),
            format!("{{\"binding_id\":\"{b}\"}}"),
        ),
        (
            JobKind::PluginCommand,
            JobScope::PluginOperation(op),
            "{}".to_string(),
        ),
    ] {
        let id = JobIdentity {
            kind: bad.0,
            scope: bad.1,
        };
        assert!(q.enqueue(id, &bad.2, 0).is_err());
        assert!(
            q.create_def(NewDef {
                kind: bad.0,
                scope: bad.1,
                name: "bad".into(),
                enabled: true,
                schedule: None,
                payload: bad.2.clone(),
                timeout_secs: None,
                concurrency_key: None,
            })
            .is_err()
        );
    }
    let mut bad_task = task_spec(a, 1, 2, true, "bad");
    bad_task.payload = format!("{{\"binding_id\":\"{b}\"}}");
    assert!(q.project_plugin_task(&bad_task).is_err());
    let mut bad_op = op_job(op, JobKind::PluginCommand, 0, Some(1), "k");
    bad_op.payload = format!("{{\"operation_id\":\"{}\"}}", PluginOperationId::new());
    assert!(q.project_plugin_operation(bad_op).is_err());

    let def_a = pa.fence.def_id.unwrap();
    let job_a = reader
        .jobs_for_identity(JobIdentity::plugin_task(a), 10)
        .unwrap()[0]
        .id;
    q.with_connection(|c| {
        c.execute(
            "UPDATE jobs SET payload='{}' WHERE id=?1",
            [job_a.to_string()],
        )?;
        c.execute(
            "UPDATE job_defs SET scope_key='global' WHERE id=?1",
            [def_a.to_string()],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .unwrap();
    assert!(reader.get(job_a).is_err());
    assert!(reader.def(def_a).is_err());
}

#[test]
fn operation_projection_is_idempotent_across_reopen_and_conflicts_on_mismatch() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let path = tmp.path().join("jobs.db");
    let op = PluginOperationId::new();
    let q = Queue::open(&path).unwrap();
    let first = q
        .project_plugin_operation(op_job(op, JobKind::PluginCommand, 0, Some(3), "rev:1"))
        .unwrap();
    assert!(first.created);
    assert_eq!(first.receipt.job_id, first.job.as_ref().unwrap().id);
    let replay = q
        .project_plugin_operation(op_job(op, JobKind::PluginCommand, 0, Some(3), "rev:1"))
        .unwrap();
    assert!(!replay.created);
    assert_eq!(replay.receipt, first.receipt);
    drop(q);

    let q = Queue::open(&path).unwrap();
    let after = q
        .project_plugin_operation(op_job(op, JobKind::PluginCommand, 0, Some(3), "rev:1"))
        .unwrap();
    assert!(!after.created);
    assert_eq!(after.receipt, first.receipt);
    let reader_conn = open_reader(&path);
    assert_eq!(
        QueueRead::new(&reader_conn)
            .plugin_operation_receipt(op, JobKind::PluginCommand, 0)
            .unwrap(),
        Some(first.receipt.clone()),
    );
    assert_eq!(
        q.jobs_for_identity(JobIdentity::plugin_command(op), 10)
            .unwrap()
            .len(),
        1
    );

    assert!(matches!(
        q.project_plugin_operation(op_job(op, JobKind::PluginCommand, 0, Some(4), "rev:1")),
        Err(JobError::Conflict(_))
    ));
    assert!(matches!(
        q.project_plugin_operation(op_job(op, JobKind::PluginCommand, 0, None, "rev:1")),
        Err(JobError::Conflict(_))
    ));
    assert!(matches!(
        q.project_plugin_operation(op_job(op, JobKind::PluginCommand, 0, Some(3), "rev:2")),
        Err(JobError::Conflict(_))
    ));

    let seq1 = q
        .project_plugin_operation(op_job(op, JobKind::PluginCommand, 1, Some(3), "rev:1"))
        .unwrap();
    let rec = q
        .project_plugin_operation(op_job(op, JobKind::PluginReconcile, 0, Some(3), "rev:1"))
        .unwrap();
    assert!(seq1.created && rec.created);
    assert_ne!(seq1.receipt.job_id, first.receipt.job_id);
    assert_ne!(rec.receipt.job_id, first.receipt.job_id);
}

#[test]
fn operation_projection_rolls_back_atomically_on_transaction_failure() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let q = Queue::open(tmp.path().join("jobs.db")).unwrap();
    let op = PluginOperationId::new();
    q.with_connection(|c| {
        c.execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON plugin_operation_receipts BEGIN SELECT RAISE(FAIL, 'injected'); END;")
    }).unwrap();
    assert!(
        q.project_plugin_operation(op_job(op, JobKind::PluginCommand, 0, Some(1), "k"))
            .is_err()
    );
    q.with_connection(|c| c.execute_batch("DROP TRIGGER fail_receipt"))
        .unwrap();
    assert!(
        q.jobs_for_identity(JobIdentity::plugin_command(op), 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        q.plugin_operation_receipt(op, JobKind::PluginCommand, 0)
            .unwrap()
            .is_none()
    );
}

#[test]
fn pruned_projected_job_retains_durable_receipt_without_recreating_effect() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let q = Queue::open(tmp.path().join("jobs.db")).unwrap();
    let (op_a, op_b) = (PluginOperationId::new(), PluginOperationId::new());
    let proj_a = q
        .project_plugin_operation(op_job(op_a, JobKind::PluginCommand, 0, Some(1), "k"))
        .unwrap();
    assert_eq!(q.claim(0, 1).unwrap().len(), 1);
    q.succeed(proj_a.receipt.job_id, 10).unwrap();
    assert_eq!(q.prune_terminal_jobs(1_000, 10, false).unwrap().deleted, 1);

    let proj_b = q
        .project_plugin_operation(op_job(op_b, JobKind::PluginCommand, 0, Some(1), "k"))
        .unwrap();
    assert_eq!(q.claim(0, 1).unwrap().len(), 1);
    q.succeed(proj_b.receipt.job_id, 10).unwrap();
    assert_eq!(
        q.prune_terminal_window("ret-def", 1_000, 10, false)
            .unwrap()
            .deleted,
        1
    );

    assert!(q.get(proj_a.receipt.job_id).unwrap().is_none());
    assert!(q.get(proj_b.receipt.job_id).unwrap().is_none());
    let re_a = q
        .project_plugin_operation(op_job(op_a, JobKind::PluginCommand, 0, Some(1), "k"))
        .unwrap();
    let re_b = q
        .project_plugin_operation(op_job(op_b, JobKind::PluginCommand, 0, Some(1), "k"))
        .unwrap();
    assert!(!re_a.created && re_a.receipt == proj_a.receipt && re_a.job.is_none());
    assert!(!re_b.created && re_b.receipt == proj_b.receipt && re_b.job.is_none());
    assert!(
        q.jobs_for_identity(JobIdentity::plugin_command(op_a), 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        q.jobs_for_identity(JobIdentity::plugin_command(op_b), 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn task_projection_fences_stale_updates_and_close_cancels_only_queued_runs() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let q = Queue::open(tmp.path().join("jobs.db")).unwrap();
    let (a, b) = (PluginTaskBindingId::new(), PluginTaskBindingId::new());
    let p1 = q
        .project_plugin_task(&task_spec(a, 1, 1, true, "v1"))
        .unwrap();
    let pb = q
        .project_plugin_task(&task_spec(b, 1, 1, true, "vb"))
        .unwrap();
    let def_a = p1.fence.def_id.unwrap();
    assert!(
        !q.project_plugin_task(&task_spec(a, 1, 1, true, "v1"))
            .unwrap()
            .applied
    );
    assert!(matches!(
        q.project_plugin_task(&task_spec(a, 1, 1, true, "v2")),
        Err(JobError::Conflict(_))
    ));

    q.ensure_scheduled_for(0, def_a).unwrap();
    let p2 = q
        .project_plugin_task(&task_spec(a, 1, 2, true, "v2"))
        .unwrap();
    assert!(p2.applied && p2.fence.def_id == Some(def_a));
    let running = q.claim(0, 1).unwrap().remove(0);
    assert!(running.payload.contains("\"v2\""));
    let queued = q.ensure_scheduled_for(0, def_a).unwrap().remove(0);

    let closed = q.close_plugin_task(a, 2, 3).unwrap();
    assert!(closed.applied && closed.fence.closed);
    assert!(!q.def(def_a).unwrap().unwrap().enabled);
    assert_eq!(
        q.get(queued.id).unwrap().unwrap().status,
        JobStatus::Cancelled
    );
    assert_eq!(
        q.get(running.id).unwrap().unwrap().status,
        JobStatus::Running
    );
    assert!(q.def(pb.fence.def_id.unwrap()).unwrap().unwrap().enabled);

    assert!(
        !q.project_plugin_task(&task_spec(a, 1, 99, true, "stale"))
            .unwrap()
            .applied
    );
    assert!(
        !q.project_plugin_task(&task_spec(a, 2, 4, true, "same-closed"))
            .unwrap()
            .applied
    );
    q.delete_def(JobIdentity::plugin_task(a)).unwrap();
    assert!(
        !q.project_plugin_task(&task_spec(a, 2, 4, true, "reopen-same"))
            .unwrap()
            .applied
    );
    assert!(
        q.def_for_identity(JobIdentity::plugin_task(a))
            .unwrap()
            .is_none()
    );

    let reopened = q
        .project_plugin_task(&task_spec(a, 3, 5, true, "v3"))
        .unwrap();
    assert!(reopened.applied && !reopened.fence.closed);
    assert!(
        q.def(reopened.fence.def_id.unwrap())
            .unwrap()
            .unwrap()
            .enabled
    );
}

#[test]
fn clearing_periodic_schedule_at_same_generation_cancels_queued_child_job() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let q = Queue::open(tmp.path().join("jobs.db")).unwrap();
    let binding = PluginTaskBindingId::new();

    let p1 = q
        .project_plugin_task(&task_spec(binding, 1, 1, true, "periodic"))
        .unwrap();
    let def_id = p1.fence.def_id.unwrap();
    let queued = q.ensure_scheduled_for(0, def_id).unwrap().remove(0);
    assert_eq!(queued.status, JobStatus::Queued);

    let mut manual_spec = task_spec(binding, 1, 2, true, "manual-only");
    manual_spec.schedule = None;
    let p2 = q.project_plugin_task(&manual_spec).unwrap();
    assert!(p2.applied);
    assert_eq!(q.def(def_id).unwrap().unwrap().schedule, None);
    assert_eq!(
        q.get(queued.id).unwrap().unwrap().status,
        JobStatus::Cancelled
    );
    assert!(q.claim(0, 10).unwrap().is_empty());
}

struct OkRunner;
impl jobs::Runner for OkRunner {
    fn run(&self, _: &jobs::Job) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn set_queued_job_payload_validates_payload_without_poisoning_tick_or_read() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let path = tmp.path().join("jobs.db");
    let q = Queue::open(&path).unwrap();
    let (binding_a, binding_b) = (PluginTaskBindingId::new(), PluginTaskBindingId::new());
    let (op_a, op_b) = (PluginOperationId::new(), PluginOperationId::new());

    let task_job = q
        .enqueue(
            JobIdentity::plugin_task(binding_a),
            &serde_json::json!({ "binding_id": binding_a, "step": 1 }).to_string(),
            0,
        )
        .unwrap();
    let cmd_job = q
        .project_plugin_operation(op_job(op_a, JobKind::PluginCommand, 0, Some(1), "k-1"))
        .unwrap()
        .job
        .unwrap();

    for bad_task_payload in [
        "not-valid-json".to_string(),
        "{}".to_string(),
        serde_json::json!({ "binding_id": binding_b }).to_string(),
    ] {
        assert!(
            q.set_queued_job_payload(task_job.id, &bad_task_payload)
                .is_err(),
            "expected error for bad task payload `{bad_task_payload}`"
        );
    }

    for bad_cmd_payload in [
        "{malformed".to_string(),
        "{}".to_string(),
        serde_json::json!({ "operation_id": op_b }).to_string(),
    ] {
        assert!(
            q.set_queued_job_payload(cmd_job.id, &bad_cmd_payload)
                .is_err(),
            "expected error for bad command payload `{bad_cmd_payload}`"
        );
    }

    // Valid queued payload update with matching identity succeeds.
    let updated_task_payload =
        serde_json::json!({ "binding_id": binding_a, "step": 2 }).to_string();
    q.set_queued_job_payload(task_job.id, &updated_task_payload)
        .unwrap();

    let reader_conn = open_reader(&path);
    let reader = QueueRead::new(&reader_conn);
    assert_eq!(
        reader.get(task_job.id).unwrap().unwrap().payload,
        updated_task_payload
    );
    assert_eq!(
        reader.get(cmd_job.id).unwrap().unwrap().payload,
        cmd_job.payload
    );

    let mutex_q = parking_lot::Mutex::new(q);
    let report = Queue::tick(&mutex_q, &OkRunner, 10);
    assert!(report.error.is_none(), "tick failed: {:?}", report.error);
    assert_eq!(report.finished.len(), 2);
}

#[test]
fn delete_def_closes_open_plugin_task_fence_against_same_generation_resurrection() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let q = Queue::open(tmp.path().join("jobs.db")).unwrap();
    let binding = PluginTaskBindingId::new();

    let p1 = q
        .project_plugin_task(&task_spec(binding, 2, 1, true, "v1"))
        .unwrap();
    assert!(p1.applied && !p1.fence.closed && p1.fence.def_id.is_some());

    // Delete def directly without calling close_plugin_task first.
    q.delete_def(JobIdentity::plugin_task(binding)).unwrap();

    // Same generation with higher revision (2, 2) must NOT resurrect a new job_def!
    let p_same_gen = q
        .project_plugin_task(&task_spec(binding, 2, 2, true, "resurrect"))
        .unwrap();
    assert!(
        !p_same_gen.applied && p_same_gen.fence.closed,
        "deleted plugin task fence in generation 2 must stay closed: {:?}",
        p_same_gen
    );
    assert!(
        q.def_for_identity(JobIdentity::plugin_task(binding))
            .unwrap()
            .is_none()
    );

    // Higher generation (3, 1) cleanly reopens!
    let p_next_gen = q
        .project_plugin_task(&task_spec(binding, 3, 1, true, "v3"))
        .unwrap();
    assert!(p_next_gen.applied && !p_next_gen.fence.closed);
}
