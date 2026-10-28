use super::*;

#[tokio::test]
async fn queued_reservation_cancel_timeout_and_panic_restore_capacity() {
    let budget = Arc::new(BlockingBudget::new(1));
    let held = budget.try_reserve().unwrap();
    let waiting_budget = budget.clone();
    let waiting = tokio::spawn(async move { waiting_budget.reserve(Duration::from_secs(1)).await });
    tokio::task::yield_now().await;
    waiting.abort();
    assert!(matches!(waiting.await, Err(error) if error.is_cancelled()));
    assert_eq!(budget.available_permits(), 0);
    assert!(matches!(
        budget.reserve(Duration::from_millis(10)).await,
        Err(BlockingError::Timeout)
    ));
    drop(held);
    let reservation = budget.reserve(Duration::from_secs(1)).await.unwrap();
    let result = reservation
        .run_to_completion(|| panic!("fixture panic"))
        .await;
    assert!(matches!(result, Err(BlockingError::Worker(_))));
    assert_eq!(budget.available_permits(), 1);
}

#[tokio::test]
async fn cancelled_request_keeps_its_acquired_permit_until_the_worker_exits() {
    let budget = Arc::new(BlockingBudget::new(1));
    let reservation = budget.reserve(Duration::from_secs(1)).await.unwrap();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let request = tokio::spawn(async move {
        reservation
            .run_to_completion(move || {
                let _ = entered.send(());
                released.recv_timeout(Duration::from_secs(3)).unwrap();
            })
            .await
    });
    started.await.unwrap();
    request.abort();
    assert!(matches!(request.await, Err(error) if error.is_cancelled()));
    let before_release = budget.available_permits();
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while budget.available_permits() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(before_release, 0);
}
