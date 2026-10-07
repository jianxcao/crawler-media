use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use domain::JobId;
use jobs::{JobKind, JobStatus, NewDef, Queue, Runner, Schedule};

fn queue() -> (tempfile::TempDir, Queue) {
    let dir = tempfile::tempdir().unwrap();
    let q = Queue::open(dir.path().join("jobs.db")).unwrap();
    (dir, q)
}

struct OkRunner;

impl Runner for OkRunner {
    fn run(&self, _job: &jobs::Job) -> Result<(), String> {
        Ok(())
    }
}

struct FailRunner;

impl Runner for FailRunner {
    fn run(&self, _job: &jobs::Job) -> Result<(), String> {
        Err("boom".into())
    }
}

struct CountingRunner(Arc<AtomicUsize>);

impl Runner for CountingRunner {
    fn run(&self, _job: &jobs::Job) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn oneshot_claim_succeeds_and_second_claim_is_empty() {
    let (_dir, q) = queue();
    let job = q.enqueue(JobKind::Transfer, "{}", 100).unwrap();
    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!(job.attempt, 0);
    assert!(job.def_id.is_none());

    let claimed = q.claim(100, 2).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, job.id);
    assert_eq!(claimed[0].status, JobStatus::Running);
    assert!(claimed[0].started_at.is_some());

    assert!(q.claim(100, 2).unwrap().is_empty());

    let done = q.succeed(job.id, 101).unwrap();
    assert_eq!(done.status, JobStatus::Succeeded);
    assert_eq!(done.finished_at, Some(101));
}

#[test]
fn future_job_is_not_claimed() {
    let (_dir, q) = queue();
    q.enqueue(JobKind::Transfer, "{}", 200).unwrap();
    assert!(q.claim(100, 2).unwrap().is_empty());
}

#[test]
fn startup_scheduling_does_not_promote_an_existing_future_child() {
    let (_dir, q) = queue();
    let def = q
        .upsert_def(NewDef {
            kind: JobKind::Transfer,
            name: "transfer".into(),
            enabled: true,
            schedule: Some(Schedule::Interval { secs: 30 }),
            payload: "{}".into(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
    let future = q.ensure_scheduled(9_999).unwrap().remove(0);

    q.ensure_scheduled(0).unwrap();

    let live = q.get_live_child(def.id).unwrap().unwrap();
    assert_eq!(live.id, future.id);
    assert_eq!(live.run_after, 9_999);
    assert!(q.claim(0, 1).unwrap().is_empty());
}

#[test]
fn cancelling_a_definition_leaves_running_external_work_live() {
    let (_dir, q) = queue();
    let def = q
        .upsert_def(NewDef {
            kind: JobKind::Transfer,
            name: "transfer".into(),
            enabled: true,
            schedule: Some(Schedule::Interval { secs: 30 }),
            payload: "{}".into(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
    let running = q.ensure_scheduled(0).unwrap().remove(0);
    q.claim(0, 1).unwrap();

    assert_eq!(q.cancel_def_children(def.id).unwrap(), 0);
    assert_eq!(
        q.get_live_child(def.id).unwrap().unwrap().status,
        JobStatus::Running
    );
    assert!(q.claim(100, 1).unwrap().is_empty());
    q.succeed(running.id, 101).unwrap();
    assert_eq!(q.get_live_child(def.id).unwrap().unwrap().run_after, 131);
}

#[test]
fn failed_job_retry_is_delayed_with_bounded_backoff() {
    let (_dir, q) = queue();
    let job = q.enqueue(JobKind::Transfer, "{}", 0).unwrap();
    let first = q.claim(0, 1).unwrap().remove(0);
    let retry = q.fail_claimed(&first, 10, "boom").unwrap().unwrap();
    assert_eq!(retry.run_after, 40);
    assert!(q.claim(39, 1).unwrap().is_empty());

    let second = q.claim(40, 1).unwrap().remove(0);
    let retry = q.fail_claimed(&second, 40, "boom").unwrap().unwrap();
    assert_eq!(retry.run_after, 100);
    assert_eq!(retry.id, job.id);
    assert!(q.claim(99, 1).unwrap().is_empty());
}

#[test]
fn long_running_failure_starts_backoff_at_failure_time() {
    let (_dir, q) = queue();
    let job = q.enqueue(JobKind::Transfer, "{}", 1_000).unwrap();
    let claimed = q.claim(1_000, 1).unwrap().remove(0);

    let retry = q
        .fail_claimed(&claimed, 1_800, "late failure")
        .unwrap()
        .unwrap();

    assert_eq!(retry.run_after, 1_830);
    assert!(q.claim(1_829, 1).unwrap().is_empty());
    assert_eq!(q.claim(1_830, 1).unwrap()[0].id, job.id);
}

#[test]
fn runner_failure_retries_until_attempt_five_stays_failed() {
    let (_dir, q) = queue();
    let job = q.enqueue(JobKind::CheckIn, "{}", 0).unwrap();
    let mut last = job;
    let mut now = 0;
    for expected in 1..=5 {
        let claimed = q.claim(now, 1).unwrap();
        assert_eq!(claimed.len(), 1, "attempt {expected}");
        last = q.fail(claimed[0].id, now, "boom").unwrap();
        assert_eq!(last.attempt, expected);
        if expected < 5 {
            assert_eq!(last.status, JobStatus::Queued);
            now = last.run_after;
        }
    }
    assert_eq!(last.status, JobStatus::Failed);
    assert_eq!(last.error.as_deref(), Some("boom"));
    assert!(q.claim(10, 2).unwrap().is_empty());
}

#[test]
fn recover_requeues_stale_running_and_fails_at_five() {
    let (_dir, q) = queue();
    let def = q
        .upsert_def(NewDef {
            kind: JobKind::Transfer,
            name: "transfer".into(),
            enabled: true,
            schedule: None,
            payload: "{}".into(),
            timeout_secs: Some(30),
            concurrency_key: None,
        })
        .unwrap();
    let job = q.enqueue(JobKind::Transfer, "{}", 0).unwrap();
    q.claim(0, 1).unwrap();

    let recovered = q.recover(31).unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].id, job.id);
    assert_eq!(recovered[0].status, JobStatus::Queued);
    assert_eq!(recovered[0].attempt, 1);

    let mut next_run = recovered[0].run_after;
    for attempt in 2..=5 {
        let started = next_run;
        q.claim(started, 1).unwrap();
        let rows = q
            .recover(started + i64::from(def.timeout_secs.unwrap()))
            .unwrap();
        assert_eq!(rows[0].attempt, attempt);
        if attempt < 5 {
            assert_eq!(rows[0].status, JobStatus::Queued);
            next_run = rows[0].run_after;
        } else {
            assert_eq!(rows[0].status, JobStatus::Failed);
        }
    }
    assert!(q.claim(10_000, 2).unwrap().is_empty());
}

#[test]
fn active_timed_out_execution_keeps_its_key_busy_but_does_not_block_other_keys() {
    let (_dir, q) = queue();
    let old_job = q
        .enqueue_with_key(JobKind::Transfer, "old", 0, Some("transfer:a"))
        .unwrap();
    let active = q.claim(0, 1).unwrap().remove(0);
    let same_key = q
        .enqueue_with_key(JobKind::Transfer, "same", 0, Some("transfer:a"))
        .unwrap();
    let unrelated = q
        .enqueue_with_key(JobKind::Transfer, "other", 0, Some("transfer:b"))
        .unwrap();

    assert!(
        q.recover_except(31, std::slice::from_ref(&active))
            .unwrap()
            .is_empty()
    );
    let claimed = q.claim(31, 2).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, unrelated.id);
    assert_eq!(
        q.get(old_job.id).unwrap().unwrap().status,
        JobStatus::Running
    );
    assert_eq!(
        q.get(same_key.id).unwrap().unwrap().status,
        JobStatus::Queued
    );

    q.succeed_claimed(&active, 32).unwrap();
    assert_eq!(q.claim(32, 1).unwrap()[0].id, same_key.id);
}

#[test]
fn stale_execution_cannot_finish_or_fail_a_reclaimed_job() {
    let (_dir, q) = queue();
    let job = q.enqueue(JobKind::Transfer, "{}", 0).unwrap();
    let first = q.claim(0, 1).unwrap().remove(0);
    q.recover(31).unwrap();
    let second = q.claim(61, 1).unwrap().remove(0);
    assert_eq!(first.id, second.id);
    assert_ne!(first.attempt, second.attempt);

    assert!(q.succeed_claimed(&first, 62).unwrap().is_none());
    assert!(
        q.fail_claimed(&first, 62, "stale failure")
            .unwrap()
            .is_none()
    );
    let running = q.get(job.id).unwrap().unwrap();
    assert_eq!(running.status, JobStatus::Running);
    assert_eq!(running.attempt, second.attempt);

    let done = q.succeed_claimed(&second, 63).unwrap().unwrap();
    assert_eq!(done.status, JobStatus::Succeeded);
}

#[test]
fn disabling_definition_cancels_already_queued_child() {
    let (_dir, q) = queue();
    let payload = r#"{"subscribe_id":"paused"}"#;
    let def = q
        .upsert_def(NewDef {
            kind: JobKind::SubscribeSearch,
            name: "search".into(),
            enabled: true,
            schedule: Some(Schedule::Interval { secs: 30 }),
            payload: payload.into(),
            timeout_secs: Some(60),
            concurrency_key: Some("subscribe:paused".into()),
        })
        .unwrap();
    let queued = q.ensure_scheduled(1_000).unwrap().remove(0);
    assert_eq!(queued.status, JobStatus::Queued);

    q.set_def_enabled_by_id(def.id, false).unwrap();

    let live = q.get_live_child(def.id).unwrap();
    assert!(
        live.is_none(),
        "disabled definition must not retain a live queued child"
    );
    let last = q.get_last_child(def.id).unwrap().unwrap();
    assert_eq!(last.id, queued.id);
    assert_eq!(last.status, JobStatus::Cancelled);
    assert!(last.finished_at.is_some());
}

#[test]
fn manually_triggering_disabled_definition_does_not_reactivate_queued_child() {
    let (_dir, q) = queue();
    let payload = r#"{}"#;
    let def = q
        .upsert_def(NewDef {
            kind: JobKind::Scrape,
            name: "Scrape".into(),
            enabled: true,
            schedule: Some(Schedule::Interval { secs: 30 }),
            payload: payload.into(),
            timeout_secs: Some(120),
            concurrency_key: Some("scrape".into()),
        })
        .unwrap();
    let queued = q.ensure_scheduled(1_000).unwrap().remove(0);
    q.set_def_enabled_by_id(def.id, false).unwrap();
    let cancelled = q.get_last_child(def.id).unwrap().unwrap();
    assert_eq!(cancelled.id, queued.id);
    assert_eq!(cancelled.status, JobStatus::Cancelled);

    q.ensure_scheduled_for(0, def.id).unwrap();

    assert!(q.get_live_child(def.id).unwrap().is_none());
    assert_eq!(
        q.get_last_child(def.id).unwrap().unwrap().status,
        JobStatus::Cancelled
    );
}

#[test]
fn disabled_def_is_not_scheduled() {
    let (_dir, q) = queue();
    let payload = r#"{"subscribe_id":"paused"}"#;
    q.upsert_def(NewDef {
        kind: JobKind::SubscribeSearch,
        name: "search".into(),
        enabled: true,
        schedule: Some(Schedule::Interval { secs: 30 }),
        payload: payload.into(),
        timeout_secs: Some(60),
        concurrency_key: Some("subscribe:paused".into()),
    })
    .unwrap();
    assert_eq!(q.ensure_scheduled(1_000).unwrap().len(), 1);
    assert_eq!(q.set_def_enabled(payload, false).unwrap(), 1);
    let def = &q.defs_for_payload(payload).unwrap()[0];
    assert!(!def.enabled);
    q.cancel_def_children(def.id).unwrap();
    assert!(q.ensure_scheduled(2_000).unwrap().is_empty());
    assert!(q.get_live_child(def.id).unwrap().is_none());
}

#[test]
fn scheduled_def_inserts_follow_up_child_after_terminal() {
    let (_dir, q) = queue();
    let def = q
        .upsert_def(NewDef {
            kind: JobKind::SubscribeSearch,
            name: "search".into(),
            enabled: true,
            schedule: Some(Schedule::Interval { secs: 30 }),
            payload: r#"{"subscribe_id":"1"}"#.into(),
            timeout_secs: Some(60),
            concurrency_key: Some("subscribe:1".into()),
        })
        .unwrap();

    let first = q.ensure_scheduled(1_000).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].def_id, Some(def.id));
    assert_eq!(first[0].run_after, 1_000);
    assert!(q.ensure_scheduled(1_000).unwrap().is_empty());

    q.claim(1_000, 1).unwrap();
    q.succeed(first[0].id, 1_005).unwrap();

    let next = q.get_live_child(def.id).unwrap();
    let next = next.expect("follow-up child");
    assert_eq!(next.status, JobStatus::Queued);
    assert_eq!(next.run_after, 1_035);
    assert_ne!(next.id, first[0].id);
}

#[test]
fn last_child_is_terminal_and_live_child_is_next() {
    let (_dir, q) = queue();
    let def = q
        .upsert_def(NewDef {
            kind: JobKind::Transfer,
            name: "transfer".into(),
            enabled: true,
            schedule: Some(Schedule::Interval { secs: 30 }),
            payload: "{}".into(),
            timeout_secs: Some(30),
            concurrency_key: Some("transfer".into()),
        })
        .unwrap();
    let first = q.ensure_scheduled(1_000).unwrap();
    q.claim(1_000, 1).unwrap();
    q.succeed(first[0].id, 1_005).unwrap();
    let last = q.get_last_child(def.id).unwrap().expect("last child");
    assert_eq!(last.id, first[0].id);
    assert_eq!(last.status, JobStatus::Succeeded);
    assert_eq!(last.finished_at, Some(1_005));
    let live = q.get_live_child(def.id).unwrap().expect("next child");
    assert_eq!(live.status, JobStatus::Queued);
    assert_eq!(live.run_after, 1_035);
}

#[test]
fn same_concurrency_key_claims_only_one_while_running() {
    let (_dir, q) = queue();
    q.enqueue_with_key(JobKind::CheckIn, "{}", 0, Some("site:a"))
        .unwrap();
    q.enqueue_with_key(JobKind::CheckIn, "{}", 0, Some("site:a"))
        .unwrap();
    let claimed = q.claim(0, 2).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].concurrency_key.as_deref(), Some("site:a"));
}

#[test]
fn different_concurrency_keys_claim_together() {
    let (_dir, q) = queue();
    q.enqueue_with_key(JobKind::CheckIn, "{}", 0, Some("site:a"))
        .unwrap();
    q.enqueue_with_key(JobKind::CheckIn, "{}", 0, Some("site:b"))
        .unwrap();
    let claimed = q.claim(0, 2).unwrap();
    assert_eq!(claimed.len(), 2);
}

#[test]
fn tick_runs_at_most_two_and_uses_injected_clock() {
    let (_dir, q) = queue();
    q.enqueue(JobKind::Transfer, "1", 50).unwrap();
    q.enqueue(JobKind::Transfer, "2", 50).unwrap();
    q.enqueue(JobKind::Transfer, "3", 50).unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let finished = q.tick(50, &CountingRunner(hits.clone())).unwrap();
    assert_eq!(finished.len(), 2);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert!(
        finished
            .iter()
            .all(|job| job.status == JobStatus::Succeeded)
    );

    let rest = q.tick(50, &OkRunner).unwrap();
    assert_eq!(rest.len(), 1);
}

#[test]
fn tick_records_runner_error_without_sleeping() {
    let (_dir, q) = queue();
    let job = q.enqueue(JobKind::Scrape, "{}", 0).unwrap();
    q.tick(0, &FailRunner).unwrap();
    let stored = q.get(job.id).unwrap().unwrap();
    assert_eq!(stored.status, JobStatus::Queued);
    assert_eq!(stored.attempt, 1);
    assert_eq!(stored.error.as_deref(), Some("boom"));
}

#[test]
fn db_lives_under_the_given_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.db");
    let _q = Queue::open(&path).unwrap();
    assert!(path.is_file());
}

#[test]
fn unknown_job_id_is_an_error() {
    let (_dir, q) = queue();
    let err = q.succeed(JobId::new(), 0).unwrap_err();
    assert!(err.to_string().contains("unknown Job"));
}

#[test]
fn ensure_def_does_not_duplicate_same_kind_and_key() {
    let (_dir, q) = queue();
    let first = q
        .ensure_def(NewDef {
            kind: JobKind::Transfer,
            name: "transfer".into(),
            enabled: true,
            schedule: Some(Schedule::Interval { secs: 30 }),
            payload: "{}".into(),
            timeout_secs: Some(30),
            concurrency_key: Some("transfer".into()),
        })
        .unwrap();
    let second = q
        .ensure_def(NewDef {
            kind: JobKind::Transfer,
            name: "transfer".into(),
            enabled: true,
            schedule: Some(Schedule::Interval { secs: 30 }),
            payload: "{}".into(),
            timeout_secs: Some(30),
            concurrency_key: Some("transfer".into()),
        })
        .unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(q.list_defs().unwrap().len(), 1);
}

#[test]
fn ensure_scheduled_for_payload_only_touches_matching_defs() {
    let (_dir, q) = queue();
    // 两个订阅 def + 一个全局 def，全部 enable 且有定时。
    for payload in ["{\"subscribe_id\":\"a\"}", "{\"subscribe_id\":\"b\"}", "{}"] {
        q.ensure_def(NewDef {
            kind: JobKind::SubscribeSearch,
            name: format!("search {payload}"),
            enabled: true,
            schedule: Some(Schedule::Interval { secs: 1800 }),
            payload: payload.to_string(),
            timeout_secs: Some(120),
            concurrency_key: None,
        })
        .unwrap();
    }
    // 先各排队一个远期任务（run_after=9999），确保手动触发前都有 live child。
    q.ensure_scheduled(9999).unwrap();
    let live_before = q
        .defs_for_payload("{\"subscribe_id\":\"a\"}")
        .unwrap()
        .into_iter()
        .map(|d| q.get_live_child(d.id).unwrap().unwrap().run_after)
        .collect::<Vec<_>>();
    assert_eq!(live_before, vec![9999]);

    // 手动触发只提前 a：b 与全局 def 的排队任务必须保持 run_after=9999。
    q.ensure_scheduled_for_payload(0, "{\"subscribe_id\":\"a\"}")
        .unwrap();
    let live_a = q
        .defs_for_payload("{\"subscribe_id\":\"a\"}")
        .unwrap()
        .into_iter()
        .map(|d| q.get_live_child(d.id).unwrap().unwrap().run_after)
        .collect::<Vec<_>>();
    assert_eq!(live_a, vec![0], "目标 def 的排队任务应立即运行");
    let untouched = ["{\"subscribe_id\":\"b\"}", "{}"].iter().all(|payload| {
        q.defs_for_payload(payload)
            .unwrap()
            .iter()
            .all(|d| q.get_live_child(d.id).unwrap().unwrap().run_after == 9999)
    });
    assert!(untouched, "其他 def 的排队任务不能被提前");
}

#[test]
fn disabled_definition_stops_automatic_retry_when_running_child_fails() {
    let (_dir, q) = queue();
    let def = q
        .ensure_def(NewDef {
            kind: JobKind::Transfer,
            name: "test-transfer".into(),
            enabled: true,
            schedule: Some(Schedule::Interval { secs: 60 }),
            payload: "{}".into(),
            timeout_secs: Some(30),
            concurrency_key: None,
        })
        .unwrap();

    q.ensure_scheduled(0).unwrap();
    let claimed = q.claim(0, 1).unwrap().remove(0);
    assert_eq!(claimed.def_id, Some(def.id));
    assert_eq!(claimed.status, JobStatus::Running);

    // 管理员停用该 definition
    q.set_def_enabled_by_id(def.id, false).unwrap();

    // 正在运行的实例失败
    let updated = q.fail_claimed(&claimed, 10, "boom").unwrap().unwrap();

    // 断言：由于定义已被停用，该任务必须直接设为终态 Failed，绝不能设回 Queued
    assert_eq!(
        updated.status,
        JobStatus::Failed,
        "当 Definition 被停用时，失败实例必须直接转为 Failed，绝不能重新 Queued 重试"
    );

    // 随后任何时间都不能再领取到该任务
    assert!(q.claim(100, 1).unwrap().is_empty());
}
