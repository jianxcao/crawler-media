use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn wait(&self) {
        let (open, _) = self
            .changed
            .wait_timeout_while(self.open.lock().unwrap(), Duration::from_secs(5), |open| {
                !*open
            })
            .unwrap();
        assert!(*open, "observation gate was never released");
    }
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

struct ReleaseOnDrop(Arc<Gate>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct ControlledFiles {
    entered: tokio::sync::mpsc::UnboundedSender<()>,
    gate: Arc<Gate>,
    panic_first: AtomicBool,
}

impl api::file_stat_read::FileStatRead for ControlledFiles {
    fn read_nfo(&self, _: &std::path::Path) -> Option<library::NfoMeta> {
        None
    }
    fn metadata(&self, path: &std::path::Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
    fn is_file(&self, path: &std::path::Path) -> bool {
        let _ = self.entered.send(());
        assert!(
            !self.panic_first.swap(false, Ordering::SeqCst),
            "fixture observation panic"
        );
        self.gate.wait();
        path.is_file()
    }
}

async fn wait_for_permit(budget: &api::blocking_budget::BlockingBudget) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while budget.available_permits() != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn cancelled_collection_observation_keeps_budget_until_worker_exits() {
    let mut f = Fixture::new();
    let budget = Arc::new(api::blocking_budget::BlockingBudget::new(1));
    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(gate.clone());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    f.state = f
        .state
        .with_http_work(budget.clone())
        .with_file_stat_read(Arc::new(ControlledFiles {
            entered: tx,
            gate: gate.clone(),
            panic_first: AtomicBool::new(false),
        }));
    runtime().block_on(async {
        let task = tokio::spawn(router(f.state.clone()).oneshot(request(
            "GET",
            &f.uri("/gallery"),
            &f.token,
            Value::Null,
        )));
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(budget.available_permits(), 0);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            budget.available_permits(),
            0,
            "cancel must not release a running worker"
        );
        gate.release();
        wait_for_permit(&budget).await;
    });
}

#[test]
fn panicked_collection_observation_releases_budget_and_can_retry() {
    let mut f = Fixture::new();
    let budget = Arc::new(api::blocking_budget::BlockingBudget::new(1));
    let gate = Arc::new(Gate::default());
    gate.release();
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();
    f.state = f
        .state
        .with_http_work(budget.clone())
        .with_file_stat_read(Arc::new(ControlledFiles {
            entered: tx,
            gate,
            panic_first: AtomicBool::new(true),
        }));
    runtime().block_on(async {
        let app = router(f.state.clone());
        let response = app
            .clone()
            .oneshot(request("GET", &f.uri("/gallery"), &f.token, Value::Null))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        wait_for_permit(&budget).await;
        let response = app
            .oneshot(request("GET", &f.uri("/gallery"), &f.token, Value::Null))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(budget.available_permits(), 1);
    });
}

#[test]
fn saturated_collection_write_leaves_no_mutation() {
    let f = Fixture::new();
    let budget = Arc::new(api::blocking_budget::BlockingBudget::new(1));
    let reservation = budget.try_reserve().unwrap();
    let state = f.state.clone().with_http_work(budget);
    let response = runtime()
        .block_on(router(state).oneshot(request(
            "PATCH",
            &f.uri(""),
            &f.token,
            json!({"name":"Late"}),
        )))
        .unwrap();
    drop(reservation);
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        f.state.store().lock().list_collections(f.user).unwrap()[0].name,
        "Original"
    );
}
