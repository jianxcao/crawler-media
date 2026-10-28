use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn coalesced_changes_apply_latest_state_and_release_owner() {
    let owner = Arc::new(ReconcileCoordinator::default());
    let runner = owner.request().unwrap();
    let latest = AtomicUsize::new(1);
    let mut applied = Vec::new();
    runner.run(|| {
        applied.push(latest.load(Ordering::SeqCst));
        if applied.len() == 1 {
            latest.store(2, Ordering::SeqCst);
            assert!(owner.request().is_none());
            latest.store(3, Ordering::SeqCst);
            assert!(owner.request().is_none());
        }
    });
    assert_eq!(applied, vec![1, 3]);
    assert!(owner.request().is_some());
}

#[test]
fn dropped_or_panicked_driver_allows_a_later_retry() {
    let owner = Arc::new(ReconcileCoordinator::default());
    drop(owner.request().unwrap());
    let runner = owner.request().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runner.run(|| panic!("fixture reconcile panic"));
    }));
    assert!(result.is_err());
    assert!(owner.request().is_some());
}

#[tokio::test]
async fn cancelled_waiter_keeps_owner_and_capacity_until_actual_worker_finishes() {
    let owner = Arc::new(ReconcileCoordinator::default());
    let runner = owner.request().unwrap();
    let budget = Arc::new(crate::blocking_budget::BlockingBudget::new(1));
    let worker_budget = budget.clone();
    let (entered, mut started) = tokio::sync::mpsc::unbounded_channel();
    let (release, released) = std::sync::mpsc::channel();
    let task = tokio::spawn(async move {
        worker_budget
            .run_to_completion(move || {
                let mut first = true;
                runner.run(|| {
                    if first {
                        first = false;
                        entered.send(()).unwrap();
                        released
                            .recv_timeout(std::time::Duration::from_secs(3))
                            .unwrap();
                    }
                })
            })
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), started.recv())
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(owner.request().is_none());
    assert_eq!(budget.available_permits(), 0);
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while budget.available_permits() != 1 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(owner.request().is_some());
}

#[tokio::test]
async fn panicked_worker_releases_owner_and_capacity() {
    let owner = Arc::new(ReconcileCoordinator::default());
    let runner = owner.request().unwrap();
    let budget = crate::blocking_budget::BlockingBudget::new(1);
    let result = budget
        .run_to_completion(move || runner.run(|| panic!("fixture panic")))
        .await;
    assert!(result.is_err());
    assert_eq!(budget.available_permits(), 1);
    assert!(owner.request().is_some());
}
