#[path = "support/remediation.rs"]
mod remediation;
#[path = "support/mod.rs"]
mod support;

#[path = "management/common.rs"]
#[allow(dead_code)]
mod common;
#[path = "storage_contention/fixtures.rs"]
#[allow(dead_code)]
mod fixtures;
#[path = "storage_contention/harness.rs"]
#[allow(dead_code)]
mod harness;

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use api::maintenance::{
    DatabaseExecutor, DatabaseOutcome, DatabaseReport, DatabaseTarget, MaintenanceContext,
};
use axum::http::StatusCode;
use jobs::{JobKind, JobStatus, Queue};
use tower::ServiceExt;

/// Database maintenance that blocks inside the executor until the test releases it:
/// a stand-in for a long VACUUM/optimize running on real storage.
struct SlowMaintenance {
    entered: std::sync::Mutex<Option<mpsc::Sender<()>>>,
    gate: remediation::Gate,
}

impl DatabaseExecutor for SlowMaintenance {
    fn execute(
        &self,
        target: &DatabaseTarget,
        _vacuum_enabled: bool,
        _context: &MaintenanceContext<'_>,
    ) -> Result<DatabaseReport, String> {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
            self.gate.wait();
        }
        Ok(DatabaseReport {
            name: target.name.as_str().to_string(),
            outcome: DatabaseOutcome::Optimized,
            free_pages: 0,
            page_size: 4096,
            size_before: 0,
            size_after: 0,
            skip_reason: None,
            elapsed_ms: 1,
        })
    }
}

async fn status_within(app: &axum::Router, token: Option<&str>) -> StatusCode {
    let request = common::request("GET", "/api/v1/libraries", token, serde_json::Value::Null);
    tokio::time::timeout(Duration::from_secs(5), app.clone().oneshot(request))
        .await
        .expect("auth must answer while slow IO and maintenance run")
        .unwrap()
        .status()
}

/// Saturate the FS budget with blocked IO and park a database maintenance job inside its
/// executor; authentication and playback heartbeats must still answer promptly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_io_and_maintenance_keep_auth_and_heartbeat_available() {
    let fixture = fixtures::fixture("Remediation Series");
    let io_gate = remediation::Gate::new();
    let _io_release = io_gate.release_guard();
    let maintenance_gate = remediation::Gate::new();
    let maintenance_release = maintenance_gate.release_guard();
    let (entered_tx, entered_rx) = mpsc::channel();
    let state = fixture
        .state
        .clone()
        .with_maintenance_executor(Arc::new(SlowMaintenance {
            entered: std::sync::Mutex::new(Some(entered_tx)),
            gate: maintenance_gate,
        }));
    let app = api::router(state.clone());

    // Slow IO: every FS permit is held by a blocked read.
    let mut io = Vec::new();
    for _ in 0..2 {
        let budget = state.fs_work.clone();
        let gate = io_gate.clone_handle();
        io.push(tokio::spawn(async move {
            budget.run_to_completion(move || gate.wait()).await
        }));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while state.fs_work.available_permits() > 0 {
        assert!(
            Instant::now() < deadline,
            "slow IO must occupy the FS budget"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let play_session =
        fixtures::open_play_session(&app, fixture.series.id, "dev-remediation").await;

    // Slow maintenance: a claimed DatabaseMaintenance job parked inside its executor.
    let job = {
        let queue = state.jobs();
        let guard = queue.lock();
        let job = guard
            .enqueue(
                JobKind::DatabaseMaintenance,
                r#"{"version":1,"vacuum_enabled":false}"#,
                0,
            )
            .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let claimed = guard.claim(now, 16).unwrap();
        claimed
            .into_iter()
            .find(|claimed| claimed.id == job.id)
            .expect("maintenance job must be claimable")
    };
    let run_state = state.clone();
    let maintenance = std::thread::spawn(move || api::maintenance::run(&run_state, &job));
    if entered_rx.recv_timeout(Duration::from_secs(10)).is_err() {
        let outcome = if maintenance.is_finished() {
            format!("{:?}", maintenance.join())
        } else {
            "still running".to_string()
        };
        panic!("maintenance must reach its executor: {outcome}");
    }

    assert_eq!(
        status_within(&app, Some(fixtures::TOKEN)).await,
        StatusCode::OK
    );
    assert_eq!(status_within(&app, None).await, StatusCode::UNAUTHORIZED);
    tokio::time::timeout(
        Duration::from_secs(5),
        fixtures::beat_start(
            &app,
            fixture.series.id,
            1_000,
            "dev-remediation",
            &play_session,
        ),
    )
    .await
    .expect("heartbeat start must answer while slow IO and maintenance run");
    tokio::time::timeout(
        Duration::from_secs(5),
        fixtures::beat(
            &app,
            fixture.series.id,
            30_000,
            "dev-remediation",
            &play_session,
        ),
    )
    .await
    .expect("heartbeat progress must answer while slow IO and maintenance run");
    assert!(
        !maintenance.is_finished(),
        "maintenance must still be running while the requests were served"
    );

    drop(maintenance_release);
    maintenance
        .join()
        .expect("maintenance thread must not panic")
        .expect("released maintenance must succeed");
    drop(_io_release);
    for task in io {
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("released IO must finish")
            .unwrap()
            .unwrap();
    }
    assert_eq!(state.fs_work.available_permits(), 2);
}

/// Cancelled requests must not free (and so expand) budget held by still-running blocking
/// work, must not leak it afterwards, and a restart must neither widen budgets nor lose
/// queued or interrupted jobs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_and_restart_do_not_expand_worker_budgets_or_lose_jobs() {
    let fixture = remediation::fixture();
    let state = fixture.state.clone();
    let gate = remediation::Gate::new();
    let release = gate.release_guard();

    let mut callers = Vec::new();
    for _ in 0..4 {
        let budget = state.search_work.clone();
        let gate = gate.clone_handle();
        callers.push(tokio::spawn(async move {
            budget
                .run(Duration::from_secs(60), move || gate.wait())
                .await
        }));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while state.search_work.available_permits() > 0 {
        assert!(
            Instant::now() < deadline,
            "workers must take the search budget"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Cancel every caller (client disconnects). The blocking work keeps running.
    for caller in &callers {
        caller.abort();
    }
    for caller in callers {
        let _ = caller.await;
    }
    assert_eq!(
        state.search_work.available_permits(),
        0,
        "cancelling callers must not hand out permits still held by running work"
    );
    assert!(
        state
            .search_work
            .run(Duration::from_secs(1), || ())
            .await
            .is_err(),
        "a new search must be refused while the budget is still occupied"
    );
    drop(release);
    let deadline = Instant::now() + Duration::from_secs(10);
    while state.search_work.available_permits() < 4 {
        assert!(
            Instant::now() < deadline,
            "finished work must return its permits"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        state.search_work.available_permits(),
        4,
        "no permit leaked or added"
    );

    // Restart: a queue with a running claim and queued work survives a reopen.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.db");
    let ids: Vec<_> = {
        let queue = Queue::open(&path).unwrap();
        let ids: Vec<_> = (0..5)
            .map(|_| {
                queue
                    .enqueue(JobKind::DatabaseMaintenance, "{}", 0)
                    .unwrap()
                    .id
            })
            .collect();
        let claimed = queue.claim(1_000, 2).unwrap();
        assert_eq!(claimed.len(), 2, "claim must respect its limit");
        ids
    }; // process "crashes" with two jobs running
    let queue = Queue::open(&path).unwrap();
    queue.recover(1_000_000).unwrap();
    for id in &ids {
        let job = queue
            .get(*id)
            .unwrap()
            .expect("no job may be lost across restart");
        assert_ne!(
            job.status,
            JobStatus::Running,
            "an interrupted claim must not stay running forever after restart: {job:?}"
        );
    }
    let queued = ids
        .iter()
        .filter(|id| queue.get(**id).unwrap().unwrap().status == JobStatus::Queued)
        .count();
    assert!(
        queued >= 3,
        "untouched queued jobs must still be queued: {queued}"
    );

    // Budgets of a restarted instance are the configured ones, not grown.
    let restarted = remediation::fixture();
    assert_eq!(restarted.state.search_work.available_permits(), 4);
    assert_eq!(restarted.state.fs_work.available_permits(), 2);
    assert_eq!(restarted.state.http_work.available_permits(), 8);
}

#[tokio::test]
async fn scoped_pages_remain_equivalent_under_background_changes() {
    let fixture = remediation::fixture();
    let (initial_status, initial) = fixture
        .request("GET", "/api/v1/libraries", Some(&fixture.member_token))
        .await;
    assert_eq!(initial_status, StatusCode::OK);
    let visible_ids = initial["data"]
        .as_array()
        .expect("library data is an array")
        .iter()
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert!(visible_ids.iter().any(|id| id == &fixture.library_id));

    // An admin-scoped read cannot change subsequent member authorization results.
    let (admin_status, _) = fixture
        .request(
            "GET",
            "/api/v1/libraries?scope=admin",
            Some(&fixture.admin_token),
        )
        .await;
    assert_eq!(admin_status, StatusCode::OK);
    let (repeat_status, repeated) = fixture
        .request("GET", "/api/v1/libraries", Some(&fixture.member_token))
        .await;
    assert_eq!(repeat_status, StatusCode::OK);
    let repeated_ids = repeated["data"]
        .as_array()
        .expect("library data is an array")
        .iter()
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert_eq!(repeated_ids, visible_ids);
}

/// R4: claiming respects its capacity and draining the queue completes every job exactly
/// once. (R3 worker capacity is covered by `chapter_frame_capacity.rs`.)
#[test]
fn r3_r4_durable_queue_capacity_and_drain_are_behaviorally_red() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path().join("jobs.db")).unwrap();
    let mut ids: Vec<_> = (0..23)
        .map(|_| {
            queue
                .enqueue(JobKind::DatabaseMaintenance, "{}", 0)
                .unwrap()
                .id
        })
        .collect();
    let mut drained = Vec::new();
    let mut rounds = 0;
    loop {
        rounds += 1;
        assert!(rounds <= 10, "drain must terminate");
        let batch = queue.claim(1_000 + rounds, 5).unwrap();
        assert!(batch.len() <= 5, "a claim must never exceed its capacity");
        if batch.is_empty() {
            break;
        }
        for job in batch {
            queue.succeed_claimed(&job, 1_000 + rounds).unwrap();
            drained.push(job.id);
        }
    }
    ids.sort_by_key(|id| format!("{id:?}"));
    drained.sort_by_key(|id| format!("{id:?}"));
    assert_eq!(drained, ids, "drain must finish every job exactly once");
    for id in ids {
        assert_eq!(queue.get(id).unwrap().unwrap().status, JobStatus::Succeeded);
    }
}
