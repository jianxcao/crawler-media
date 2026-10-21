use jobs::{JobKind, NewDef, Queue, Schedule};
use rusqlite::params;
use serde_json::json;

#[test]
fn rewrite_media_refs_updates_def_queued_and_running_payloads_and_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("jobs.db");
    let queue = Queue::open(&db_path).unwrap();

    let from_id = "00000000-0000-0000-0000-000000000001";
    let to_id = "00000000-0000-0000-0000-000000000002";
    let unrelated_id = "00000000-0000-0000-0000-000000000099";

    // 1. Definition payload
    let def = queue
        .upsert_def(NewDef {
            kind: JobKind::CatalogRefresh,
            name: "refresh-def".into(),
            schedule: Some(Schedule::Interval { secs: 3600 }),
            enabled: true,
            payload: json!({ "media_id": from_id, "extra": "keep" }).to_string(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();

    // 2. Queued job
    let queued_job = queue
        .enqueue(
            JobKind::CatalogRefresh,
            &json!({ "media_id": from_id, "step": 1 }).to_string(),
            0,
        )
        .unwrap();

    // 3. Running job (claim it)
    let running_job = queue
        .enqueue(
            JobKind::CatalogRefresh,
            &json!({ "media_id": from_id, "step": 2 }).to_string(),
            0,
        )
        .unwrap();
    let claimed = queue.claim(100, 10).unwrap();
    assert!(claimed.iter().any(|j| j.id == running_job.id));

    // 4. Succeeded job (terminal status)
    let term_job = queue
        .enqueue(
            JobKind::CatalogRefresh,
            &json!({ "media_id": from_id, "step": 3 }).to_string(),
            0,
        )
        .unwrap();
    let term_claimed = queue.claim(100, 10).unwrap();
    let term_job_claimed = term_claimed.iter().find(|j| j.id == term_job.id).unwrap();
    queue.succeed_claimed(term_job_claimed, 200).unwrap();

    // 5. Unrelated def and job
    let unrelated_def = queue
        .upsert_def(NewDef {
            kind: JobKind::CatalogRefresh,
            name: "unrelated-def".into(),
            schedule: Some(Schedule::Interval { secs: 3600 }),
            enabled: true,
            payload: json!({ "media_id": unrelated_id, "note": "leave alone" }).to_string(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
    let _unrelated_queued = queue
        .enqueue(
            JobKind::CatalogRefresh,
            &json!({ "media_id": unrelated_id }).to_string(),
            0,
        )
        .unwrap();

    // 6. Non-json or non-matching payload
    let non_json_def = queue
        .upsert_def(NewDef {
            kind: JobKind::CatalogRefresh,
            name: "non-json-def".into(),
            schedule: Some(Schedule::Interval { secs: 3600 }),
            enabled: true,
            payload: "raw string payload".to_string(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
    let _non_json_queued = queue
        .enqueue(JobKind::CatalogRefresh, "raw string job payload", 0)
        .unwrap();

    // Run rewrite_media_refs: should update:
    // - 1 in job_defs (def)
    // - 2 in jobs (queued_job + running_job)
    // Total = 3
    let updated = queue.rewrite_media_refs(from_id, to_id).unwrap();
    assert_eq!(updated, 3);

    // Second call is idempotent and returns 0
    let second_call = queue.rewrite_media_refs(from_id, to_id).unwrap();
    assert_eq!(second_call, 0);

    // Verify database contents
    let conn = rusqlite::Connection::open(&db_path).unwrap();

    // Def payload updated
    let def_payload: String = conn
        .query_row(
            "SELECT payload FROM job_defs WHERE id = ?1",
            params![def.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let def_json: serde_json::Value = serde_json::from_str(&def_payload).unwrap();
    assert_eq!(def_json["media_id"], to_id);
    assert_eq!(def_json["extra"], "keep");

    // Queued job payload updated
    let queued_payload: String = conn
        .query_row(
            "SELECT payload FROM jobs WHERE id = ?1",
            params![queued_job.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let queued_json: serde_json::Value = serde_json::from_str(&queued_payload).unwrap();
    assert_eq!(queued_json["media_id"], to_id);

    // Running job payload updated
    let running_payload: String = conn
        .query_row(
            "SELECT payload FROM jobs WHERE id = ?1",
            params![running_job.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let running_json: serde_json::Value = serde_json::from_str(&running_payload).unwrap();
    assert_eq!(running_json["media_id"], to_id);

    // Succeeded job payload was NOT updated
    let term_payload: String = conn
        .query_row(
            "SELECT payload FROM jobs WHERE id = ?1",
            params![term_job.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let term_json: serde_json::Value = serde_json::from_str(&term_payload).unwrap();
    assert_eq!(term_json["media_id"], from_id);

    // Unrelated payload untouched
    let unrelated_payload: String = conn
        .query_row(
            "SELECT payload FROM job_defs WHERE id = ?1",
            params![unrelated_def.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let unrelated_json: serde_json::Value = serde_json::from_str(&unrelated_payload).unwrap();
    assert_eq!(unrelated_json["media_id"], unrelated_id);

    // Non-json untouched
    let non_json_str: String = conn
        .query_row(
            "SELECT payload FROM job_defs WHERE id = ?1",
            params![non_json_def.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(non_json_str, "raw string payload");
}
