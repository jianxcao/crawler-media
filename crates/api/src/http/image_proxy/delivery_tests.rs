//! Terminal delivery regressions use finite transports and real response bytes.
use super::super::{
    ProxyQuery,
    cache::ImageCache,
    runtime::{DeliveryError, ImageProxy},
};
use axum::body::to_bytes;
use axum::http::{HeaderMap, StatusCode};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

mod cache_read_fifo {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/cache_read_fifo.rs"
    ));
}

const BYTES: &[u8] = &[9, 8, 7, 6];
struct Source {
    gate: (Mutex<bool>, Condvar),
    delay: Duration,
    active: AtomicUsize,
    peak: AtomicUsize,
}
impl Source {
    fn new(gated: bool, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            gate: (Mutex::new(!gated), Condvar::new()),
            delay,
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }
    fn release(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
    fn fetch(&self) -> Result<(String, Vec<u8>), DeliveryError> {
        struct Active<'a>(&'a AtomicUsize);
        impl Drop for Active<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let _active = Active(&self.active);
        let (open, timeout) = self
            .gate
            .1
            .wait_timeout_while(
                self.gate.0.lock().unwrap(),
                Duration::from_secs(2),
                |open| !*open,
            )
            .unwrap();
        if timeout.timed_out() && !*open {
            return Err("finite source timed out".into());
        }
        drop(open);
        std::thread::sleep(self.delay);
        Ok(("image/png".into(), BYTES.to_vec()))
    }
}
struct Release(Arc<Source>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
fn query(url: &str) -> ProxyQuery {
    ProxyQuery { url: url.into() }
}
async fn until(ready: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(1);
    while !ready() {
        assert!(Instant::now() < end, "finite proxy work did not finish");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
async fn assert_image(response: axum::response::Response) {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(response.headers()["cache-control"], "public, max-age=86400");
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
        BYTES
    );
}

#[tokio::test]
async fn earlier_proxy_observer_keeps_original_deadline_when_joining_later_fill() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let source = Source::new(true, Duration::ZERO);
    let _release = Release(source.clone());
    let fetch = source.clone();
    let gate = Arc::new(Semaphore::new(1));
    let proxy = Arc::new(ImageProxy::new(
        Arc::new(ImageCache::new(tmp.path().into(), 1000, 100)),
        gate.clone(),
        Arc::new(move |_| fetch.fetch()),
    ));
    let url = "https://image.tmdb.org/earlier-observer.png";
    let path = tmp
        .path()
        .join(format!("{}.img", super::super::cache_key(url)));
    let (cache_read, entered) = cache_read_fifo::CacheReadPause::new(&path);
    let started = Instant::now();
    let earlier_proxy = proxy.clone();
    let earlier =
        tokio::spawn(async move { earlier_proxy.response(query(url), HeaderMap::new()).await });
    entered.await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let later_proxy = proxy.clone();
    let later =
        tokio::spawn(async move { later_proxy.response(query(url), HeaderMap::new()).await });
    until(|| source.active.load(Ordering::SeqCst) == 1).await;
    cache_read.finish();
    let earlier_result = tokio::time::timeout_at(
        tokio::time::Instant::from_std(started + Duration::from_millis(2250)),
        earlier,
    )
    .await;
    let active = source.active.load(Ordering::SeqCst);
    let permits = gate.available_permits();
    let later_pending = !later.is_finished();
    source.release();
    let later_response = later.await.unwrap();
    until(|| gate.available_permits() == 1 && source.active.load(Ordering::SeqCst) == 0).await;
    let earlier_response = earlier_result
        .expect("joining a later flight must not extend the earlier HTTP deadline")
        .unwrap();
    assert_eq!(earlier_response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(active, 1);
    assert_eq!(permits, 0, "real shared worker must retain its permit");
    assert!(later_pending);
    assert_image(later_response).await;
}

#[tokio::test]
async fn delayed_proxy_cold_fill_returns_the_completed_image_and_conditional_cache_hit() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let gate = Arc::new(Semaphore::new(1));
    let source = Source::new(false, Duration::from_millis(250));
    let fetch = source.clone();
    let proxy = Arc::new(ImageProxy::new(
        Arc::new(ImageCache::new(tmp.path().into(), 1000, 100)),
        gate.clone(),
        Arc::new(move |_| fetch.fetch()),
    ));
    let url = "https://image.tmdb.org/finite-delayed.png";
    let response = proxy.response(query(url), HeaderMap::new()).await;
    let etag = response.headers().get("etag").cloned();
    until(|| gate.available_permits() == 1).await;
    assert_image(response).await;
    let etag = etag.expect("the first completed image needs its cache validator");
    let _busy = gate.acquire().await.unwrap();
    assert_image(proxy.response(query(url), HeaderMap::new()).await).await;
    let mut headers = HeaderMap::new();
    headers.insert("if-none-match", etag);
    let response = proxy.response(query(url), headers).await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert!(
        to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn concurrent_proxy_cold_observers_share_one_terminal_fill() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let source = Source::new(true, Duration::ZERO);
    let _release = Release(source.clone());
    let fetch = source.clone();
    let gate = Arc::new(Semaphore::new(1));
    let proxy = Arc::new(ImageProxy::new(
        Arc::new(ImageCache::new(tmp.path().into(), 1000, 100)),
        gate.clone(),
        Arc::new(move |_| fetch.fetch()),
    ));
    let observers: Vec<_> = (0..8)
        .map(|_| {
            let proxy = proxy.clone();
            tokio::spawn(async move {
                proxy
                    .response(
                        query("https://image.tmdb.org/finite-shared.png"),
                        HeaderMap::new(),
                    )
                    .await
            })
        })
        .collect();
    until(|| source.active.load(Ordering::SeqCst) == 1).await;
    tokio::time::sleep(Duration::from_millis(180)).await;
    let pending = observers.iter().all(|observer| !observer.is_finished());
    source.release();
    let mut responses = Vec::new();
    for observer in observers {
        responses.push(observer.await.unwrap());
    }
    until(|| gate.available_permits() == 1 && source.active.load(Ordering::SeqCst) == 0).await;
    assert!(
        pending,
        "cold observers must await the shared terminal image"
    );
    assert_eq!(source.peak.load(Ordering::SeqCst), 1);
    for response in responses {
        assert_image(response).await;
    }
}

#[tokio::test]
async fn proxy_upstream_failure_is_a_terminal_gateway_error() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let gate = Arc::new(Semaphore::new(1));
    let proxy = Arc::new(ImageProxy::new(
        Arc::new(ImageCache::new(tmp.path().into(), 1000, 100)),
        gate.clone(),
        Arc::new(|_| Err("finite upstream failure".into())),
    ));
    let response = proxy
        .response(
            query("https://image.tmdb.org/finite-failure.png"),
            HeaderMap::new(),
        )
        .await;
    until(|| gate.available_permits() == 1).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("retry-after"));
}

#[tokio::test]
async fn proxy_waits_for_capacity_that_returns_before_the_fill_deadline() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let gate = Arc::new(Semaphore::new(1));
    let busy = gate.clone().acquire_owned().await.unwrap();
    let source = Source::new(false, Duration::ZERO);
    let fetch = source.clone();
    let proxy = Arc::new(ImageProxy::new(
        Arc::new(ImageCache::new(tmp.path().into(), 1000, 100)),
        gate.clone(),
        Arc::new(move |_| fetch.fetch()),
    ));
    let observer = tokio::spawn(async move {
        proxy
            .response(
                query("https://image.tmdb.org/finite-queued.png"),
                HeaderMap::new(),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(40)).await;
    drop(busy);
    let response = tokio::time::timeout(Duration::from_secs(1), observer)
        .await
        .unwrap()
        .unwrap();
    until(|| gate.available_permits() == 1 && source.active.load(Ordering::SeqCst) == 0).await;
    assert_image(response).await;
}

#[tokio::test]
async fn expired_proxy_queue_never_starts_a_late_fill() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let gate = Arc::new(Semaphore::new(1));
    let busy = gate.clone().acquire_owned().await.unwrap();
    let source = Source::new(false, Duration::ZERO);
    let fetch = source.clone();
    let cache = Arc::new(ImageCache::new(tmp.path().into(), 1000, 100));
    let proxy = Arc::new(ImageProxy::new(
        cache.clone(),
        gate.clone(),
        Arc::new(move |_| fetch.fetch()),
    ));
    let url = "https://image.tmdb.org/finite-expired-queue.png";
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        proxy.response(query(url), HeaderMap::new()),
    )
    .await;
    drop(busy);
    tokio::time::sleep(Duration::from_millis(50)).await;
    until(|| gate.available_permits() == 1 && source.active.load(Ordering::SeqCst) == 0).await;
    let response = result.expect("queueing must have a bounded terminal response");
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(source.peak.load(Ordering::SeqCst), 0);
    assert!(
        cache.lookup(url).unwrap().is_none(),
        "expired queue work must not later publish an image"
    );
}
