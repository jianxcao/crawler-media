//! Database maintenance borrows the same unique writer as normal mutations.
use api::maintenance::{
    DatabaseExecutor, DatabaseName, DatabaseOutcome, DatabaseReport, DatabaseTarget,
    MaintenanceContext, SqliteExecutor, database_targets, run,
};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::{
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

#[path = "maintenance_support/mod.rs"]
mod support;

#[test]
fn maintenance_stop_check_reads_while_the_jobs_writer_is_held() {
    let (_temp, state) = support::temp_state(Arc::new(SqliteExecutor));
    let job = support::enqueue_claim(
        &state,
        jobs::JobKind::DatabaseMaintenance,
        support::DB_LIGHT,
        support::unix_now(),
    );
    let jobs = state.jobs();
    let writer = jobs.lock();
    let (result_tx, result_rx) = mpsc::channel();
    let check = std::thread::spawn(move || {
        let result = api::maintenance::check_maintenance_stop(&state, &job);
        let _ = result_tx.send(result);
    });
    let result = result_rx.recv_timeout(Duration::from_millis(200));
    drop(writer);
    check.join().unwrap();
    assert_eq!(
        result.expect("read-only maintenance admission waited for the Jobs writer"),
        api::maintenance::MaintenanceStop::Active
    );
}

#[test]
fn jobs_retention_dry_run_uses_readers_and_never_begins_a_writer_transaction() {
    let (_temp, state) = support::temp_state(Arc::new(SqliteExecutor));
    let now = support::unix_now();
    let terminal = support::enqueue_claim(
        &state,
        jobs::JobKind::JobsRetention,
        "{}",
        now - 10 * support::DAY,
    );
    state
        .jobs()
        .lock()
        .succeed(terminal.id, now - 10 * support::DAY)
        .unwrap();
    let job = support::enqueue_claim(&state, jobs::JobKind::JobsRetention, support::PREVIEW, now);
    state.jobs().lock().with_connection(|conn| {
        conn.authorizer(Some(|ctx: AuthContext<'_>| {
            if matches!(ctx.action, AuthAction::Transaction { .. }) {
                Authorization::Deny
            } else {
                Authorization::Allow
            }
        }))
    });
    let result = run(&state, &job);
    let report = support::progress_of(&state, &job);
    state
        .jobs()
        .lock()
        .with_connection(|conn| conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>));
    assert!(
        result.is_ok(),
        "read-only retention preview entered the writer transaction: {result:?}; {report}"
    );
    assert_eq!(report["candidates"], 1, "{report}");
    assert_eq!(report["deleted"], 0, "{report}");
    assert!(state.jobs().lock().get(terminal.id).unwrap().is_some());
}

struct ObservedExecutor(mpsc::Sender<DatabaseName>);

impl DatabaseExecutor for ObservedExecutor {
    fn execute(
        &self,
        _conn: &rusqlite::Connection,
        target: &DatabaseTarget,
        _vacuum_enabled: bool,
        _context: &MaintenanceContext<'_>,
    ) -> Result<DatabaseReport, String> {
        self.0.send(target.name).unwrap();
        Ok(DatabaseReport {
            name: target.name.as_str().into(),
            outcome: DatabaseOutcome::Optimized,
            free_pages: 0,
            page_size: 4096,
            size_before: 0,
            size_after: 0,
            skip_reason: None,
            elapsed_ms: 0,
        })
    }
}

fn refuse_optimize(context: AuthContext<'_>) -> Authorization {
    if matches!(
        context.action,
        AuthAction::Pragma {
            pragma_name: "optimize",
            ..
        }
    ) {
        Authorization::Deny
    } else {
        Authorization::Allow
    }
}

#[test]
fn real_maintenance_obeys_policy_on_every_actual_writer_connection() {
    let (_temp, state) = support::temp_state(Arc::new(SqliteExecutor));
    for db in [
        store::Db::App,
        store::Db::Subscribe,
        store::Db::Library,
        store::Db::Cache,
    ] {
        state
            .store()
            .with_writer(db, |conn| {
                conn.authorizer(Some(refuse_optimize));
                Ok(())
            })
            .unwrap();
    }
    state
        .jobs()
        .lock()
        .with_connection(|conn| conn.authorizer(Some(refuse_optimize)));
    let job = support::enqueue_claim(
        &state,
        jobs::JobKind::DatabaseMaintenance,
        support::DB_LIGHT,
        support::unix_now(),
    );
    let result = run(&state, &job);
    let report = support::progress_of(&state, &job);
    for db in [
        store::Db::App,
        store::Db::Subscribe,
        store::Db::Library,
        store::Db::Cache,
    ] {
        state
            .store()
            .with_writer(db, |conn| {
                conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
                Ok(())
            })
            .unwrap();
    }
    state
        .jobs()
        .lock()
        .with_connection(|conn| conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>));
    assert!(
        result.is_err(),
        "maintenance bypassed its writer connection's authorization policy: {report}"
    );
    for name in ["app", "subscribe", "library", "cache", "jobs"] {
        assert_eq!(
            support::db_entry(&report, name)["outcome"],
            "failed",
            "{report}"
        );
    }
}

#[test]
fn sqlite_maintenance_restores_busy_timeout_and_preserves_progress_policy() {
    let (_temp, state) = support::temp_state(Arc::new(SqliteExecutor));
    let store = state.store();
    let target = database_targets(store.data_dir(), store.app_db_path())
        .into_iter()
        .find(|t| t.name == DatabaseName::App)
        .unwrap();
    let interrupt = Arc::new(AtomicBool::new(false));
    let progress = interrupt.clone();
    store.with_writer(store::Db::App, |conn| {
        conn.busy_timeout(Duration::from_millis(1379))?;
        conn.progress_handler(1, Some(move || progress.load(Ordering::Acquire)));
        let context = MaintenanceContext {
            now: support::unix_now(), last_vacuum_success: None,
            vacuum_history_unreadable: false, cancelled: &|| false,
        };
        let report = SqliteExecutor.execute(conn, &target, false, &context).unwrap();
        assert_eq!(report.outcome, DatabaseOutcome::Optimized);
        let timeout: i64 = conn.pragma_query_value(None, "busy_timeout", |row| row.get(0))?;
        assert_eq!(timeout, 1379, "maintenance shortened the shared writer's foreground policy");
        interrupt.store(true, Ordering::Release);
        let result = conn.query_row("SELECT 42", [], |row| row.get::<_, i64>(0));
        conn.progress_handler(0, None::<fn() -> bool>);
        assert!(matches!(result, Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == rusqlite::ffi::ErrorCode::OperationInterrupted), "maintenance replaced the writer's progress policy");
        Ok(())
    }).unwrap();
}

#[test]
fn maintenance_waits_for_the_actual_library_writer() {
    let (called_tx, called_rx) = mpsc::channel();
    let (_temp, state) = support::temp_state(Arc::new(ObservedExecutor(called_tx)));
    let job = support::enqueue_claim(
        &state,
        jobs::JobKind::DatabaseMaintenance,
        support::DB_LIGHT,
        support::unix_now(),
    );
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let store = state.store();
    let writer = std::thread::spawn(move || {
        store
            .with_writer(store::Db::Library, |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
            .unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let maintenance = std::thread::spawn(move || run(&state, &job));
    assert_eq!(
        called_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        DatabaseName::App
    );
    assert_eq!(
        called_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        DatabaseName::Cache
    );
    let attempted_while_held = called_rx.recv_timeout(Duration::from_millis(200));
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    maintenance.join().unwrap().unwrap();
    assert!(
        attempted_while_held.is_err(),
        "maintenance bypassed the held writer: {attempted_while_held:?}"
    );
    assert_eq!(
        called_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        DatabaseName::Library
    );
}

/// 首个数据库维护持有自己的 writer 时，其余库与读池仍可用。
struct BlockingExecutor {
    entered: mpsc::Sender<()>,
    release: StdMutex<mpsc::Receiver<()>>,
    blocked: std::sync::atomic::AtomicBool,
}

impl DatabaseExecutor for BlockingExecutor {
    fn execute(
        &self,
        _conn: &rusqlite::Connection,
        target: &DatabaseTarget,
        _vacuum_enabled: bool,
        _context: &MaintenanceContext<'_>,
    ) -> Result<DatabaseReport, String> {
        if !self.blocked.swap(true, std::sync::atomic::Ordering::SeqCst) {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("test must release the blocked maintenance executor");
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

#[test]
fn maintenance_holds_only_its_database_writer() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (tmp, state) = support::temp_state(Arc::new(BlockingExecutor {
        entered: entered_tx,
        release: StdMutex::new(release_rx),
        blocked: std::sync::atomic::AtomicBool::new(false),
    }));
    assert!(tmp.path().exists());
    let job = support::enqueue_claim(
        &state,
        jobs::JobKind::DatabaseMaintenance,
        support::DB_LIGHT,
        support::unix_now(),
    );

    let run_state = state.clone();
    let handle = std::thread::spawn(move || run(&run_state, &job));
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("执行器必须被调用");

    assert!(
        state
            .store()
            .with_writer(store::Db::Cache, |_| Ok(()))
            .is_ok(),
        "App 维护期间其他数据库 writer 继续可用"
    );
    assert!(
        state.store().get_setting("instance.default_locale").is_ok(),
        "App 维护期间读池继续可用"
    );
    assert!(
        state.jobs().try_lock().is_some(),
        "维护执行期间不得持有 Queue 锁"
    );

    release_tx.send(()).unwrap();
    handle.join().unwrap().unwrap();
}
