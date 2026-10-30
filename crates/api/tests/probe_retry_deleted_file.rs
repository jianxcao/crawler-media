//! S01: a due retry whose file no longer exists must retire the stage without
//! re-locking the Store it is already holding. Each scenario runs in a child
//! process watched by an external deadline so a regression reports a hang
//! instead of wedging the whole test binary.
#[path = "probe_retry_support.rs"]
mod support;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use store::{ProbeStage, ProbeStageCompletion, ProbeStageKey};
use support::ProbeScenario;

const CHILD_ENV: &str = "PROBE_RETRY_DELETED_FILE_CHILD";
const DEADLINE: Duration = Duration::from_secs(60);

/// Run `name` in a child process; fail if it does not finish before the deadline.
fn watched(name: &str) {
    if std::env::var(CHILD_ENV).as_deref() == Ok(name) {
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([name, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, name)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "{name}: child failed: {status}");
            return;
        }
        if started.elapsed() > DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{name}: dispatch did not finish within {DEADLINE:?} (Store self-deadlock)");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn is_child(name: &str) -> bool {
    std::env::var(CHILD_ENV).as_deref() == Ok(name)
}

/// Record a failed, due Metadata stage for `ledger_id` via the public seam.
fn failed_stage(s: &ProbeScenario, ledger_id: String) -> ProbeStageKey {
    let key = ProbeStageKey {
        ledger_id,
        context_key: "ctx-deleted".into(),
        stage: ProbeStage::Metadata,
    };
    let store = s.store.as_ref();
    assert!(store.claim_probe_stage(&key, "job-1", 1_000_000).unwrap());
    assert!(
        store
            .finish_probe_stage(
                &key,
                "job-1",
                &ProbeStageCompletion::Failed {
                    error_kind: "io".into(),
                    error: "boom".into(),
                },
                1_000_000,
            )
            .unwrap()
    );
    key
}

fn dispatch_at_deadline_and_check(s: &ProbeScenario, key: &ProbeStageKey) {
    let deadline = s
        .store
        .as_ref()
        .get_probe_stage(key)
        .unwrap()
        .unwrap()
        .next_retry_at_ms
        .expect("failed stage has a retry deadline");
    let dispatched = s.manager.dispatch_due(deadline, 32);
    assert_eq!(dispatched.unwrap(), 0);
    s.store
        .writers()
        .with_writer_timeout(Duration::from_secs(1), store::Db::Library, |_| Ok(()))
        .expect("Library writer must be free after dispatch");
    assert!(s.store.get_probe_stage(key).unwrap().is_none());
}

#[test]
fn retry_of_absent_file_retires_and_releases_store() {
    const NAME: &str = "retry_of_absent_file_retires_and_releases_store";
    if !is_child(NAME) {
        return watched(NAME);
    }
    let s = ProbeScenario::cached_season();
    let key = failed_stage(&s, domain::LedgerId::new().to_string());
    dispatch_at_deadline_and_check(&s, &key);
}

#[test]
fn file_deleted_after_failure_retires_and_releases_store() {
    const NAME: &str = "file_deleted_after_failure_retires_and_releases_store";
    if !is_child(NAME) {
        return watched(NAME);
    }
    let s = ProbeScenario::cached_season();
    let row = &s.ledgers[0];
    let file_id = row.file.file_id;
    let removed = s
        .store
        .as_ref()
        .delete_files(row.file.library_id, &[file_id])
        .unwrap();
    assert_eq!(removed, 1);
    // The retry row outlives the file (claimed concurrently with the delete).
    let key = failed_stage(&s, file_id.to_string());
    dispatch_at_deadline_and_check(&s, &key);
}
