//! Maintenance HTTP reads preserve backpressure and cancel active Jobs SQL.
use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use serde_json::Value;
use std::num::NonZeroUsize;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tower::ServiceExt;

const TOKEN: &str = "jobs-reader-failures";

struct Offline;
impl Fetcher for Offline {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline Jobs reader fixture".into()))
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    state: ApiState,
    uri: String,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let state = ApiState::new(
            Store::open(temp.path().join("data")).unwrap(),
            TOKEN.into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(Offline),
            Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
            temp.path().join("library"),
        )
        .unwrap();
        let def = state
            .jobs()
            .lock()
            .list_defs()
            .unwrap()
            .into_iter()
            .find(|def| def.kind == jobs::JobKind::JobsRetention)
            .unwrap();
        Self {
            _temp: temp,
            state,
            uri: format!("/api/v1/jobs/{}/maintenance-preview", def.id),
        }
    }
    fn request(&self) -> Request<Body> {
        Request::post(&self.uri)
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap()
    }
    async fn preview(&self) -> (StatusCode, Value) {
        let response = router(self.state.clone())
            .oneshot(self.request())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn maintenance_preview_reports_jobs_reader_saturation_and_recovers() {
    let fixture = Fixture::new();
    let readers = fixture.state.jobs_readers();
    let first = readers.try_checkout().unwrap();
    let second = readers.try_checkout().unwrap();
    runtime().block_on(async {
        let (status, body) = fixture.preview().await;
        drop((first, second));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(body["error"]["code"], "runtime.busy");
        let (status, body) = fixture.preview().await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["kind"], "jobs_retention");
    });
}

#[test]
fn maintenance_preview_reports_closed_jobs_readers_as_unavailable() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        assert_eq!(fixture.preview().await.0, StatusCode::OK);
        fixture.state.jobs_readers().close();
        let (status, body) = fixture.preview().await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(body["error"]["code"], "runtime.busy");
    });
}

#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}
impl Gate {
    fn pause(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.0 = true;
        self.changed.notify_all();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(5), |state| !state.1)
            .unwrap();
        let released = state.1;
        drop(state);
        assert!(released, "test must release its SQL gate");
    }
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !self
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .0
            {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("Jobs SQL must reach its progress boundary");
    }
    fn release(&self) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .1 = true;
        self.changed.notify_all();
    }
}
struct ReleaseOnDrop(Arc<Gate>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[test]
fn cancelling_maintenance_preview_interrupts_sql_and_returns_its_reader() {
    let fixture = Fixture::new();
    let readers = fixture.state.jobs_readers();
    readers.set_capacity(NonZeroUsize::new(1).unwrap()).unwrap();
    let progress = Arc::new(Gate::default());
    let select = Arc::new(Gate::default());
    let _release_progress = ReleaseOnDrop(progress.clone());
    let _release_select = ReleaseOnDrop(select.clone());
    // Cancel at a real query progress boundary. A successful abandoned read
    // would proceed into a second SQL gate during its post-work validation;
    // interruption must instead clean up and return its actual reader.
    let mut setup = readers.try_checkout().unwrap();
    let select_gate = select.clone();
    let query_entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let validation_entered = query_entered.clone();
    setup
        .with_connection(
            &store::QueryBudget {
                deadline: Instant::now() + Duration::from_secs(5),
                cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            |conn| {
                conn.authorizer(Some(move |ctx: AuthContext<'_>| {
                    if validation_entered.load(std::sync::atomic::Ordering::Acquire)
                        && matches!(
                            ctx.action,
                            AuthAction::Pragma {
                                pragma_name: "application_id",
                                ..
                            }
                        )
                    {
                        select_gate.pause();
                    }
                    Authorization::Allow
                }));
                Ok(())
            },
        )
        .unwrap();
    drop(setup);
    let progress_gate = progress.clone();
    readers.set_query_progress_observer(Some(Arc::new(move || {
        query_entered.store(true, std::sync::atomic::Ordering::Release);
        progress_gate.pause();
    })));
    runtime().block_on(async {
        let request = tokio::spawn(router(fixture.state.clone()).oneshot(fixture.request()));
        progress.entered().await;
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        readers.set_query_progress_observer(None);
        progress.release();
        let returned = tokio::time::timeout(
            Duration::from_millis(500),
            readers.checkout_async(Duration::from_secs(1)),
        )
        .await;
        // Release even the rejected path before an assertion can unwind the runtime.
        select.release();
        let mut next = returned
            .expect(
                "cancelled Jobs SQL must return capacity before its original two-second deadline",
            )
            .unwrap();
        next.with_connection(
            &store::QueryBudget {
                deadline: Instant::now() + Duration::from_secs(1),
                cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            |conn| {
                conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
                Ok(conn.query_row("SELECT count(*) FROM job_defs", [], |row| {
                    row.get::<_, i64>(0)
                })?)
            },
        )
        .unwrap();
        drop(next);
        let (status, body) = fixture.preview().await;
        assert_eq!(status, StatusCode::OK, "{body}");
    });
}
