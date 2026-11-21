//! P01: background retry dispatch only serves the current probe context.
//! Superseded contexts are retired durably and can neither occupy the due
//! window nor be revived by a late worker or a restart.
#[path = "probe_retry_support.rs"]
mod support;

use api::probe_manager::clock::ProbeClock;
use api::probe_manager::policy::{ProbeRequestOrigin, ProbeRequestResult};
use store::{ProbeStage, ProbeStageCompletion, ProbeStageKey};
use support::ProbeScenario;

fn raw(s: &ProbeScenario) -> rusqlite::Connection {
    rusqlite::Connection::open(s.store.as_ref().data_dir().join("library.db")).unwrap()
}

/// `n` failed outro rows of superseded contexts on `file_id`, due before `due_ms`.
fn stale_failures(s: &ProbeScenario, file_id: &str, n: usize, due_ms: i64) {
    let conn = raw(s);
    for i in 0..n {
        conn.execute(
            "INSERT INTO probe_stage_state
                 (file_id, context_key, stage, status, failure_count, next_retry_at_ms, updated_at_ms)
             VALUES (?1, ?2, 'outro', 'failed', 1, ?3, 0)",
            rusqlite::params![file_id, format!("superseded-{i:04}"), due_ms - 1 - i as i64],
        )
        .unwrap();
    }
}

fn stage_rows(s: &ProbeScenario, file_id: &str) -> Vec<String> {
    let conn = raw(s);
    let mut stmt = conn
        .prepare(
            "SELECT context_key FROM probe_stage_state WHERE file_id = ?1 ORDER BY context_key",
        )
        .unwrap();
    stmt.query_map([file_id], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn file_id(s: &ProbeScenario, idx: usize) -> String {
    s.ledgers[idx].file.file_id.to_string()
}

/// E08 has a real, current-context outro failure due at 1_060_000.
async fn current_failure(s: &mut ProbeScenario) {
    s.set_time_ms(1_000_000);
    s.missing_outro(8);
    s.fail_next_outro(8, "HTTP error 403 Forbidden");
    s.request(8, ProbeRequestOrigin::Detail);
    s.drain().await;
}

#[tokio::test]
async fn superseded_contexts_ahead_cannot_starve_a_current_retry() {
    for stale in [32, 1000] {
        let mut s = ProbeScenario::cached_season();
        current_failure(&mut s).await;
        // E01's cache is complete for its current context; its old contexts all
        // fall due before E08's real retry.
        stale_failures(&s, &file_id(&s, 0), stale, 1_060_000);
        s.set_time_ms(1_060_000);
        let mut queued = 0;
        for _ in 0..4 {
            queued += s.dispatch_due();
        }
        assert_eq!(
            queued, 1,
            "{stale} superseded rows ahead: the current retry must be queued"
        );
        s.drain().await;
        assert_eq!(s.outro_reads(8), 2);
        assert_eq!(s.outro_reads(1), 0, "superseded contexts never re-sample");
        assert!(
            stage_rows(&s, &file_id(&s, 0))
                .iter()
                .all(|k| !k.starts_with("superseded-"))
        );
    }
}

#[tokio::test]
async fn success_in_the_current_context_retires_older_contexts() {
    let mut s = ProbeScenario::cached_season();
    current_failure(&mut s).await;
    let e08 = file_id(&s, 7);
    stale_failures(&s, &e08, 3, 1_060_000);
    // The next attempt of the current context succeeds.
    *s.engine.outro_error.lock() = None;
    s.set_time_ms(1_060_000);
    assert_eq!(s.dispatch_due(), 1);
    s.drain().await;
    let rows = stage_rows(&s, &e08);
    assert!(
        rows.iter().all(|k| !k.starts_with("superseded-")),
        "{rows:?}"
    );
    s.set_time_ms(i64::MAX / 4);
    assert_eq!(s.dispatch_due(), 0);
    assert_eq!(
        s.request(8, ProbeRequestOrigin::Detail),
        ProbeRequestResult::Complete
    );
}

#[tokio::test]
async fn late_worker_and_restart_cannot_revive_a_retired_context() {
    let mut s = ProbeScenario::cached_season();
    current_failure(&mut s).await;
    let e01 = file_id(&s, 0);
    stale_failures(&s, &e01, 1, 1_060_000);
    s.set_time_ms(1_060_000);
    s.dispatch_due();
    s.drain().await;
    let key = ProbeStageKey {
        ledger_id: e01.clone(),
        context_key: "superseded-0000".into(),
        stage: ProbeStage::Outro,
    };
    let late = s.store.as_ref().finish_probe_stage(
        &key,
        "late-job",
        &ProbeStageCompletion::Failed {
            error_kind: "http_403".into(),
            error: "late".into(),
        },
        1_060_001,
    );
    assert!(
        !late.unwrap(),
        "late worker of a retired context must not write"
    );
    s.reopen().await;
    s.set_time_ms(i64::MAX / 4);
    s.dispatch_due();
    assert!(
        stage_rows(&s, &e01)
            .iter()
            .all(|k| !k.starts_with("superseded-"))
    );
}

#[tokio::test]
async fn claimed_retry_is_not_redispatched_every_tick() {
    let mut s = ProbeScenario::cached_season();
    current_failure(&mut s).await;
    s.set_time_ms(1_060_000);
    // Hold the worker: the claim alone must keep the next ticks from re-hitting it.
    assert_eq!(s.dispatch_due(), 1);
    assert_eq!(s.dispatch_due(), 0);
    s.set_time_ms(1_061_000);
    assert_eq!(s.dispatch_due(), 0);
    s.drain().await;
}

fn probe_unit_spec(ledger_id: &str) -> store::ProbeJobUnitSpec<'_> {
    store::ProbeJobUnitSpec {
        ledger_id,
        kind: "tv",
        force_fingerprint: false,
        reuse_fingerprint_cache: true,
        overwrite_markers: false,
        reuse_media_info_cache: true,
    }
}

#[tokio::test]
async fn reader_pool_busy_during_recovery_preserves_unit_and_deleted_file_is_cancelled() {
    use std::sync::Arc;
    use std::time::Duration;

    let s = ProbeScenario::cached_season();
    s.manager.shutdown_workers().await.unwrap();
    s.store
        .as_ref()
        .put_setting("probe.remote_reads", r#"{"paused":false}"#)
        .unwrap();
    let kept_str = s.ledgers[0].file.file_id.to_string();
    let deleted_str = domain::LedgerId::new().to_string();
    let media_str = s.ledgers[0].file.media_id.to_string();

    // Persist a valid queued unit and a unit whose file has already been removed.
    let job_id = "recovery-busy-job";
    assert!(
        s.store
            .as_ref()
            .create_probe_job(
                job_id,
                "probe",
                &media_str,
                Some(1),
                "manual-probe:recovery-busy",
                &[probe_unit_spec(&kept_str), probe_unit_spec(&deleted_str)],
            )
            .unwrap()
    );

    // Exhaust all Library readers right after `active_probe_unit_refs` succeeds on a
    // fresh ProbeManager before workers start, so `file_coverage_by_id` fails with ReadBusy.
    let pool = s.store.as_ref().reader_pool(store::Db::Library);
    let held_leases: Arc<parking_lot::Mutex<Vec<store::ReaderLease>>> =
        Arc::new(parking_lot::Mutex::new(Vec::new()));
    let held_for_hook = Arc::clone(&held_leases);
    let pool_for_hook = Arc::clone(&pool);
    // Construct without running recovery yet by holding the readers during explicit recovery.
    let restarted = Arc::new(
        api::probe_manager::ProbeManager::with_engines_unrecovered(
            s.store.clone(),
            s.engine.clone(),
            s.engine.clone(),
        )
        .unwrap(),
    );
    restarted.set_clock(s.clock.clone());
    let mgr = Arc::clone(&restarted);
    let busy_run = tokio::task::spawn_blocking(move || {
        mgr.recover_pending_jobs_observed(|| {
            let mut leases = Vec::new();
            while let Ok(lease) = pool_for_hook.try_checkout() {
                leases.push(lease);
            }
            assert!(!leases.is_empty(), "must exhaust the Library reader pool");
            *held_for_hook.lock() = leases;
        });
    });
    busy_run.await.unwrap();
    held_leases.lock().clear();

    // While readers were temporarily busy, neither unit may have been cancelled.
    assert_eq!(
        s.store
            .as_ref()
            .active_probe_unit_for_ledger(&kept_str)
            .unwrap()
            .expect("busy reader must not cancel valid probe unit")
            .status,
        "queued"
    );

    // Release readers and start workers: the deferred recovery loop must automatically
    // retry, cancel the genuinely deleted file, and recover & complete the valid unit.
    assert!(restarted.try_start_workers());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let units = s.store.as_ref().probe_job_units(job_id).unwrap();
        let kept_done = units
            .iter()
            .find(|u| u.ledger_id == kept_str)
            .is_some_and(|u| u.status == "succeeded");
        let deleted_cancelled = units
            .iter()
            .find(|u| u.ledger_id == deleted_str)
            .is_some_and(|u| u.status == "cancelled");
        if kept_done && deleted_cancelled {
            break;
        }
        if units
            .iter()
            .any(|u| u.stage == store::ProbeDispatchStage::Analysis)
        {
            s.clock.set(s.clock.now_ms() + 2_000);
            restarted.wake_dispatch();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "recovery did not converge: {units:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    restarted.shutdown_workers().await.unwrap();
}
