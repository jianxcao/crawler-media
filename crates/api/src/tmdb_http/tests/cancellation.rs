use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

use media::{CatalogGet, CatalogRequest, TmdbError};
use tokio::sync::oneshot;
use ureq::{Body, config::IpFamily, http::Response};

use super::super::{retry::Runtime, transport::Transport};
use super::PATH;
use super::support::{Fixture, Step};
use crate::blocking_budget::{BlockingBudget, BlockingError};

#[test]
fn expired_and_cancelled_requests_never_start_get() {
    let fixture = Fixture::new(vec![Step::status(200)]);
    let expired = CatalogRequest::new(Instant::now() - Duration::from_secs(1));
    assert!(matches!(
        expired.scope(|| fixture.http.get(PATH)),
        Err(TmdbError::Deadline)
    ));

    let cancelled = CatalogRequest::new(Instant::now() + Duration::from_secs(30));
    cancelled.cancel();
    assert!(matches!(
        cancelled.scope(|| fixture.http.get(PATH)),
        Err(TmdbError::Cancelled)
    ));
}

struct CancelOnSleep {
    request: CatalogRequest,
    start: Instant,
    elapsed: Mutex<Duration>,
}

impl Runtime for CancelOnSleep {
    fn now(&self) -> Instant {
        self.start + *self.elapsed.lock().unwrap()
    }
    fn wall_now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
    fn jitter(&self, ceiling: Duration) -> Duration {
        ceiling
    }
    fn sleep(&self, delay: Duration) {
        *self.elapsed.lock().unwrap() += delay;
        self.request.cancel();
    }
}

#[test]
fn cancellation_during_backoff_stops_before_the_next_get() {
    let request = CatalogRequest::new(Instant::now() + Duration::from_secs(30));
    let mut fixture = Fixture::new(vec![Step::status(500), Step::status(200)]);
    fixture.http.runtime = Arc::new(CancelOnSleep {
        request: request.clone(),
        start: Instant::now(),
        elapsed: Mutex::new(Duration::ZERO),
    });
    assert!(matches!(
        request.scope(|| fixture.http.get(PATH)),
        Err(TmdbError::Cancelled)
    ));
}

struct GateTransport {
    gate: Mutex<Option<mpsc::Receiver<()>>>,
    entered: Mutex<Option<oneshot::Sender<()>>>,
}

impl Transport for GateTransport {
    fn send(
        &self,
        _url: &str,
        _proxy: Option<&str>,
        _family: IpFamily,
        _timeout: Duration,
    ) -> Result<Response<Body>, ureq::Error> {
        let gate = self.gate.lock().unwrap().take();
        if let Some(gate) = gate {
            if let Some(entered) = self.entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            let _ = gate.recv_timeout(Duration::from_secs(5));
            return Ok(Response::builder()
                .status(500)
                .body(Body::builder().data("failed"))
                .unwrap());
        }
        Ok(Response::builder()
            .status(200)
            .body(Body::builder().data("unexpected success"))
            .unwrap())
    }
}

async fn caller_gone_stops_blocking_catalog_work(reserved: bool, timeout: bool) {
    let mut fixture = Fixture::new(Vec::new());
    let (release_tx, release_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = oneshot::channel();
    let (result_tx, result_rx) = oneshot::channel();
    fixture.http.transport = Arc::new(GateTransport {
        gate: Mutex::new(Some(release_rx)),
        entered: Mutex::new(Some(entered_tx)),
    });
    let budget = Arc::new(BlockingBudget::new(1));
    let caller_budget = budget.clone();
    let caller = tokio::spawn(async move {
        let work = move || {
            let result = fixture.http.get(PATH);
            let _ = result_tx.send(result);
        };
        let duration = if timeout {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(5)
        };
        if reserved {
            caller_budget
                .try_reserve()
                .unwrap()
                .run(duration, work)
                .await
        } else {
            caller_budget.run(duration, work).await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), entered_rx)
        .await
        .unwrap()
        .unwrap();
    if timeout {
        assert!(matches!(caller.await.unwrap(), Err(BlockingError::Timeout)));
    } else {
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
    }
    release_tx.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), result_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(result, Err(TmdbError::Cancelled | TmdbError::Deadline)),
        "{result:?}"
    );
}

#[tokio::test]
async fn dropped_caller_cancels_running_catalog_for_both_budget_paths() {
    caller_gone_stops_blocking_catalog_work(false, false).await;
    caller_gone_stops_blocking_catalog_work(true, false).await;
}

#[tokio::test]
async fn elapsed_caller_deadline_stops_running_catalog_for_both_budget_paths() {
    caller_gone_stops_blocking_catalog_work(false, true).await;
    caller_gone_stops_blocking_catalog_work(true, true).await;
}
