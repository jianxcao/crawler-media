//! L5: 维护期关闸（写门禁）时，前台写入口立刻 503 + `Retry-After`，而不是在
//! SQLite busy 上耗满 `busy_timeout` 再报 500。
//!
//! 断言走公开 seam：`maintenance::run`（注入阻塞的执行器）+ 三条真实写路由
//! （播放进度、订阅、Library 写入）。执行器用 entered/release 通道同步，不靠 sleep。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, mpsc};
use std::time::{Duration, Instant};

use api::maintenance::{
    DatabaseExecutor, DatabaseOutcome, DatabaseReport, DatabaseTarget, MaintenanceContext, run,
};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use jobs::JobKind;
use serde_json::{Value, json};
use tower::ServiceExt;

#[path = "maintenance_support/mod.rs"]
mod support;
use support::{DB_VACUUM, TOKEN, enqueue_claim, temp_state, unix_now};

/// 模拟独占的 VACUUM 阶段：第一次进入后阻塞，直到测试放行。
struct BlockingVacuum {
    entered: mpsc::Sender<()>,
    release: StdMutex<mpsc::Receiver<()>>,
    blocked: AtomicBool,
}

impl DatabaseExecutor for BlockingVacuum {
    fn execute(
        &self,
        target: &DatabaseTarget,
        _vacuum_enabled: bool,
        _context: &MaintenanceContext<'_>,
    ) -> Result<DatabaseReport, String> {
        if !self.blocked.swap(true, Ordering::SeqCst) {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .expect("测试必须放行被阻塞的维护执行器");
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

/// `/playback/progress` 的 `StrictJson` 提取先于 handler 体，所以要发形状合法的
/// 载荷；门禁关着时仍然必须是 503（handler 第一行就是门禁检查）。
fn playback_body() -> Value {
    json!({
        "device_id": "dev-1",
        "play_session_id": "session-not-issued",
        "event_sequence": 1,
        "event": "progress",
        "media_item_id": "movie:00000000-0000-0000-0000-000000000000",
    })
}

fn body_for(uri: &str) -> Value {
    if uri.ends_with("/playback/progress") {
        playback_body()
    } else {
        json!({})
    }
}

fn request(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// 三条真实写入口。请求体故意不合法：门禁关上时"繁忙"优先于"参数错误"，
/// 客户端重试才有意义；门禁开着时它们落到各自的 4xx。
const WRITE_ENTRIES: [(&str, &str); 3] = [
    ("POST", "/api/v1/playback/progress"),
    ("POST", "/api/v1/subscriptions"),
    ("POST", "/api/v1/libraries"),
];

async fn status_of(app: &axum::Router, method: &str, uri: &str) -> (StatusCode, Option<String>) {
    let response = app
        .clone()
        .oneshot(request(method, uri, body_for(uri)))
        .await
        .unwrap();
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    (response.status(), retry_after)
}

async fn error_code(app: &axum::Router, method: &str, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(request(method, uri, body_for(uri)))
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playback_progress_gets_503_during_vacuum() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (_tmp, state) = temp_state(Arc::new(BlockingVacuum {
        entered: entered_tx,
        release: StdMutex::new(release_rx),
        blocked: AtomicBool::new(false),
    }));
    let app = api::router(state.clone());

    // 基线：门禁开着，三条写入口各自落到参数校验（4xx），都不是 503。
    let mut baseline: Vec<StatusCode> = Vec::new();
    for (method, uri) in WRITE_ENTRIES {
        let (status, _) = status_of(&app, method, uri).await;
        assert_ne!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "门禁开着时 {method} {uri} 不应报繁忙"
        );
        baseline.push(status);
    }

    // 维护任务进入独占阶段（执行器阻塞在 execute 内），写门禁此时必须关闭。
    let job = enqueue_claim(&state, JobKind::DatabaseMaintenance, DB_VACUUM, unix_now());
    let run_state = state.clone();
    let handle = std::thread::spawn(move || run(&run_state, &job));
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("维护执行器必须被调用");

    for (method, uri) in WRITE_ENTRIES {
        let started = Instant::now();
        let (status, retry_after) = status_of(&app, method, uri).await;
        let elapsed = started.elapsed();
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "维护关闸期间 {method} {uri} 必须立刻回 503，而不是等 SQLite busy"
        );
        assert_eq!(
            retry_after.as_deref(),
            Some("1"),
            "{method} {uri} 的 503 必须带 Retry-After"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "{method} {uri} 关闸响应耗时 {elapsed:?}，说明仍在等 SQLite busy"
        );
    }
    // 同一套繁忙契约：`runtime.busy`（与身份读取繁忙一致），不是"参数错误"。
    assert_eq!(
        error_code(&app, "POST", "/api/v1/playback/progress").await["error"]["code"],
        "runtime.busy"
    );

    release_tx.send(()).unwrap();
    handle.join().unwrap().unwrap();

    // 门禁随维护结束打开：同一批写入口逐条回到基线状态。
    for ((method, uri), expected) in WRITE_ENTRIES.into_iter().zip(baseline) {
        let (status, _) = status_of(&app, method, uri).await;
        assert_eq!(
            status, expected,
            "维护结束后 {method} {uri} 必须恢复到与基线一致的状态"
        );
    }
}
