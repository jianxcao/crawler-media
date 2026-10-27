//! Task 27：身份读取走 app.db 的**专用读连接**，不再排在进程级 `Store` 锁后面。
//!
//! 背景（回归现场）：Task 7 把 `authenticate` 移出执行器并给了 1 s 的锁预算。方向对，
//! 但一次一万集的扫描每批都拿锁写库、写完立刻再拿，读侧被饿住超过 1 s 就判繁忙——
//! **扫描期间每个成员请求都 503**，心跳在内。这里锁三件事：
//!
//! 1. 另一线程把 `Store` 锁按住**超过旧预算**时，成员请求仍然完成鉴权（不是「礼貌地
//!    503」，而是真的 200），且耗时远小于旧预算；
//! 2. 没有 token 时仍然是 401——鉴权中间件照常工作，不是整条路径被短路；
//! 3. 真正的繁忙（读连接拿不到一致快照 / 预算已过期）仍然是 503 `runtime.busy`，
//!    这个错误码没有被删掉。

use std::sync::Arc;
use std::time::{Duration, Instant};

use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::provider::QueryTiming;
use serde_json::Value;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

const TOKEN: &str = "management-secret";

fn fixture() -> (tempfile::TempDir, ApiState) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let state = ApiState::new(
        store,
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap();
    (dir, state)
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri)
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("X-Emby-Device-Id", "auth-reader")
        .body(Body::empty())
        .unwrap()
}

async fn json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// 把 `Store` 锁按住 `hold` 时长，返回「已持锁」信号。
fn hold_store_lock(state: &ApiState, hold: Duration) -> std::sync::mpsc::Receiver<()> {
    let store = state.store();
    let (ready, wait) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let guard = store.lock();
        ready.send(()).unwrap();
        std::thread::sleep(hold);
        drop(guard);
    });
    wait
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn member_request_authenticates_while_store_lock_is_held_past_the_old_budget() {
    let (_dir, state) = fixture();
    let app = router(state.clone());
    // 旧预算 1 s；按住 2 s 保证「超过旧预算」是事实，而不是时序巧合。
    let holding = hold_store_lock(&state, Duration::from_millis(2_000));
    holding
        .recv_timeout(Duration::from_secs(5))
        .expect("扫描线程必须先进到持锁状态");

    for uri in ["/api/v1/auth/me", "/api/v1/auth/me"] {
        let started = Instant::now();
        let response = app.clone().oneshot(get(uri)).await.unwrap();
        let elapsed = started.elapsed();
        let status = response.status();
        let body = json(response).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{uri} 在 Store 锁被按住时必须仍然鉴权成功（不是 503）: {body}"
        );
        assert_eq!(body["data"]["login"], "admin");
        println!("AUTH_READER_UNDER_HELD_LOCK: {uri} elapsed={elapsed:?} status={status}");
        // 读连接不排队：耗时必须是「单行读 + 一次 blocking 交接」的量级，
        // 而不是等到锁释放（那会是 ≥2 s）。
        assert!(
            elapsed < Duration::from_millis(1_000),
            "{uri} 在持锁期间耗时 {elapsed:?}，说明它仍在等 Store 锁"
        );
    }

    // 没有 token 时仍然是 401：中间件正常执行，不是整条路径被短路。
    let unauthorized = app
        .clone()
        .oneshot(Request::get("/api/v1/auth/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn expired_auth_budget_still_answers_runtime_busy() {
    let (_dir, state) = fixture();
    let library = state
        .store()
        .lock()
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    // 库浏览路由带 `QueryTiming`：预算已过期时 auth 阶段立即判繁忙。
    let started = Instant::now() - Duration::from_millis(100);
    let mut request = get(&format!("/api/v1/libraries/{library}/items?limit=1"));
    request.extensions_mut().insert(QueryTiming {
        started,
        deadline: started + Duration::from_millis(50),
    });
    let response = router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = json(response).await;
    assert_eq!(
        body["error"]["code"], "runtime.busy",
        "真正的繁忙仍是 503 runtime.busy，不能退化成身份错误: {body}"
    );
}

/// 旧路径的实测形状：同一个 `Store` 锁被按住时，`try_lock_for(1 s)` 会**等满**整个
/// 预算再放弃，上层据此 503。这条用例把「修复前每个请求付出的代价」量出来，
/// 与新读连接（同一时刻几毫秒内成功）形成同场景 A/B。
#[test]
fn old_store_lock_path_burns_the_whole_auth_budget_when_the_lock_is_held() {
    let (_dir, state) = fixture();
    let budget = Duration::from_millis(1_000);
    let holding = hold_store_lock(&state, budget + Duration::from_millis(500));
    holding
        .recv_timeout(Duration::from_secs(5))
        .expect("扫描线程必须先进到持锁状态");
    let started = Instant::now();
    let store = state.store();
    let acquired = store.try_lock_for(budget).is_some();
    let waited = started.elapsed();
    println!("OLD_LOCK_PATH: try_lock_for({budget:?}) -> acquired={acquired} waited={waited:?}");
    assert!(
        !acquired,
        "持锁期间 try_lock_for 必然拿不到——这就是旧路径 503 的来源"
    );
    assert!(
        waited >= Duration::from_millis(900),
        "旧路径会等满整个预算（实测 {waited:?}），期间请求只能 503"
    );
}
