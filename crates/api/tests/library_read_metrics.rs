use api::{ApiState, Store, router};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::{provider::QueryTiming, query_metrics::QueryMetrics};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}
fn fixture() -> (tempfile::TempDir, ApiState, domain::LibraryId) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let library = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap();
    (dir, state, library.parse().unwrap())
}
fn request(uri: &str, timing: QueryTiming, metrics: QueryMetrics) -> Request<Body> {
    let mut req = Request::get(uri)
        .header("Authorization", "Bearer management-secret")
        .header("X-Emby-Device-Id", "secret-binding-value")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(timing);
    req.extensions_mut().insert(metrics);
    req
}

#[tokio::test]
async fn actual_request_metrics_preserve_short_timing_origin_and_final_counts() {
    let (_dir, state, library) = fixture();
    let started = Instant::now() - Duration::from_millis(40);
    let timing = QueryTiming {
        started,
        deadline: started + Duration::from_secs(5),
    };
    for uri in [
        format!("/api/v1/libraries/{library}/items?limit=100"),
        format!("/Items?ParentId={library}&Limit=100"),
        "/Users/me/Items/Latest?Limit=100".into(),
    ] {
        let metrics = QueryMetrics::new(timing, true);
        let response = router(state.clone())
            .oneshot(request(&uri, timing, metrics.clone()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let s = metrics.snapshot();
        assert!(s.elapsed_ms >= 40. && s.elapsed_ms < 5000., "{s:?}");
        assert!(s.auth_ms > 0., "{s:?}");
        // Task 27：两套表面（`/api/v1` 与 Jellyfin `/Items`、`/Users/...`）的身份读
        // 都走 app.db 的专用读连接，**不再拿进程级 `Store` 锁**，所以 `auth_wait`
        // 这一段恒为 0。断言 0 而不是「> 0」是**收紧**：哪天它又变成非零，就说明
        // 身份读退回了排队等锁的老路（扫描期间每个成员请求都 503 的那个 bug）。
        assert_eq!(
            s.auth_lock_wait_ms, 0.,
            "身份读不得再等 Store 锁（Task 27）: {s:?}"
        );
        assert!(
            s.prepare_lock_held_ms > 0. && s.validation_lock_held_ms > 0.,
            "{s:?}"
        );
        assert_eq!(s.returned, Some(0), "{s:?}");
        assert_eq!(
            s.total,
            if uri.contains("/Latest?") {
                None
            } else {
                Some(0)
            },
            "Latest omits an exact total; paged endpoints retain it: {s:?}"
        );
        assert!(
            s.vm_steps.unwrap() > 0 && s.statements.unwrap() > 0,
            "{s:?}"
        );
        assert!(!format!("{s:?}").contains("secret-binding-value"));
    }
}

#[tokio::test]
async fn distinct_requests_with_same_deadline_do_not_share_measurements() {
    let (_dir, state, library) = fixture();
    let timing = QueryTiming::start();
    let success = QueryMetrics::new(timing, false);
    let rejected = QueryMetrics::new(timing, false);
    let valid = request(
        &format!("/api/v1/libraries/{library}/facets"),
        timing,
        success.clone(),
    );
    let mut invalid = request("/Items?Limit=0", timing, rejected.clone());
    invalid
        .headers_mut()
        .insert("Authorization", "Bearer invalid".parse().unwrap());
    let app = router(state);
    let (ok, denied) = tokio::join!(app.clone().oneshot(valid), app.oneshot(invalid));
    assert_eq!(ok.unwrap().status(), StatusCode::OK);
    assert_eq!(denied.unwrap().status(), StatusCode::UNAUTHORIZED);
    assert_ne!(
        success.snapshot().request_id,
        rejected.snapshot().request_id
    );
    assert_eq!(success.snapshot().total, Some(0));
    assert_eq!(rejected.snapshot().total, None);
    assert!(success.snapshot().sql_ms > 0.);
    assert_eq!(rejected.snapshot().sql_ms, 0.);
    assert_eq!(
        success.snapshot().vm_steps,
        None,
        "ordinary tracing stays disabled"
    );
}

#[tokio::test]
async fn expired_pre_auth_budget_reports_actual_origin_and_stage_without_page_counts() {
    let (_dir, state, _) = fixture();
    let started = Instant::now() - Duration::from_millis(100);
    let timing = QueryTiming {
        started,
        deadline: started + Duration::from_millis(50),
    };
    let metrics = QueryMetrics::new(timing, false);
    let response = router(state)
        .oneshot(request("/Items?Limit=0", timing, metrics.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let s = metrics.snapshot();
    assert_eq!(s.timeout_stage, Some("auth"));
    assert_eq!(s.timeouts, 1);
    assert_eq!(s.remaining_budget_ms, Some(0.));
    assert_eq!(s.returned, None);
    assert_eq!(s.total, None);
    assert!(s.elapsed_ms >= 100. && s.elapsed_ms < 5000.);
}

#[tokio::test]
async fn pool_rejection_has_no_fabricated_page_or_sql_work_and_recovery_is_measured() {
    let (_dir, state, library) = fixture();
    let state = state
        .with_library_read_pool_size(std::num::NonZeroUsize::new(1).unwrap())
        .unwrap();
    let pool = state.library_read_pool_weak().upgrade().unwrap();
    let lease = pool.try_checkout().unwrap();
    let timing = QueryTiming::start();
    let metrics = QueryMetrics::new(timing, true);
    let uri = format!("/api/v1/libraries/{library}/facets");
    let response = router(state.clone())
        .oneshot(request(&uri, timing, metrics.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let s = metrics.snapshot();
    assert_eq!(s.pool_rejections, 1);
    assert_eq!(s.returned, None);
    assert_eq!(s.total, None);
    assert_eq!(s.vm_steps, None);
    drop(lease);
    let timing = QueryTiming::start();
    let fresh = QueryMetrics::new(timing, true);
    let response = router(state)
        .oneshot(request(&uri, timing, fresh.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fresh.snapshot().pool_rejections, 0);
    assert!(fresh.snapshot().vm_steps.unwrap() > 0);
}
