//! A durable root queue has one finite in-process driver. Weak ownership lets it
//! stop when the instance is dropped; notifications are hints, not the progress log.
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::management::ApiState;

pub(crate) fn notify(state: &ApiState) {
    state.library_work.root_changed.notify_one();
}

pub(crate) fn start(state: &ApiState) {
    if state.library_work.root_driver.swap(true, Ordering::AcqRel) {
        return;
    }
    let store = std::sync::Arc::downgrade(&state.store);
    let budget = std::sync::Arc::downgrade(&state.fs_work);
    let changed = state.library_work.root_changed.clone();
    tokio::spawn(async move {
        loop {
            let (Some(shared), Some(budget)) = (store.upgrade(), budget.upgrade()) else {
                break;
            };
            let result = budget
                .run_to_completion(move || shared.lock().drain_one_root_batch())
                .await;
            match result {
                Ok(Ok(Some(true))) => {
                    tokio::task::yield_now().await;
                    continue;
                }
                Ok(Err(error)) => {
                    tracing::error!(%error,"Root reassignment window failed; durable work retained")
                }
                Err(crate::blocking_budget::BlockingError::Busy) => {}
                Err(error) => tracing::error!(%error,"Root reassignment worker failed"),
                _ => {}
            }
            drop(budget);
            tokio::select! {
                _=changed.notified()=>{},
                _=tokio::time::sleep(Duration::from_millis(250))=>{},
            }
        }
    });
}
