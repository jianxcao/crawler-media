use super::cache::ImageCache;
use std::sync::{Arc, Barrier};

fn image(bytes: usize) -> Result<(String, Vec<u8>), String> {
    Ok(("image/png".into(), vec![42; bytes]))
}

fn disk_bytes(root: &std::path::Path) -> u64 {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum()
}

#[test]
fn cache_reuses_complete_images_without_fetching_again() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ImageCache::new(tmp.path().into(), 100, 40);
    assert_eq!(cache.get("one", || image(20)).unwrap().1, vec![42; 20]);
    assert_eq!(
        cache.get("one", || Err("must not fetch".into())).unwrap().1,
        vec![42; 20]
    );
}

#[test]
fn oversized_response_is_rejected_without_cache_write() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ImageCache::new(tmp.path().into(), 100, 40);
    assert!(cache.get("large", || image(41)).is_err());
    assert_eq!(disk_bytes(tmp.path()), 0);
}

#[test]
fn eviction_makes_room_before_publication_and_failed_fetch_keeps_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ImageCache::new(tmp.path().into(), 50, 40);
    cache.get("one", || image(30)).unwrap();
    cache.get("two", || image(30)).unwrap();
    assert!(disk_bytes(tmp.path()) <= 50);
    assert!(
        cache
            .get("failed", || Err("upstream failed".into()))
            .is_err()
    );
    assert!(disk_bytes(tmp.path()) <= 50);
    assert_eq!(
        cache
            .get("two", || Err("cache lost".into()))
            .unwrap()
            .1
            .len(),
        30
    );
}

#[test]
fn deletion_failure_rejects_new_image_without_exceeding_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ImageCache::with_remover(
        tmp.path().into(),
        50,
        40,
        Arc::new(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "fixture refuses deletion",
            ))
        }),
    );
    cache.get("one", || image(30)).unwrap();
    let error = cache.get("two", || image(30)).unwrap_err();
    assert!(error.contains("fixture refuses deletion"), "{error}");
    assert!(disk_bytes(tmp.path()) <= 50);
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
}

#[test]
fn concurrent_writers_publish_complete_files_within_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Arc::new(ImageCache::new(tmp.path().into(), 100, 40));
    let barrier = Arc::new(Barrier::new(8));
    std::thread::scope(|scope| {
        for index in 0..8 {
            let cache = cache.clone();
            let barrier = barrier.clone();
            scope.spawn(move || {
                cache
                    .get(&format!("image-{index}"), || {
                        barrier.wait();
                        image(30)
                    })
                    .unwrap();
            });
        }
    });
    assert!(disk_bytes(tmp.path()) <= 100);
    for entry in std::fs::read_dir(tmp.path()).unwrap() {
        let entry = entry.unwrap();
        assert_eq!(entry.path().extension().unwrap(), "img");
        assert_eq!(
            std::fs::read(entry.path()).unwrap(),
            [b"image/png\n".as_slice(), &[42; 30]].concat()
        );
    }
}

#[test]
fn concurrent_same_url_writes_never_expose_partial_images() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Arc::new(ImageCache::new(tmp.path().into(), 100, 40));
    let barrier = Arc::new(Barrier::new(8));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let cache = cache.clone();
            let barrier = barrier.clone();
            scope.spawn(move || {
                let result = cache
                    .get("same", || {
                        barrier.wait();
                        image(30)
                    })
                    .unwrap();
                assert_eq!(result.1, vec![42; 30]);
            });
        }
    });
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
    assert_eq!(
        cache
            .get("same", || Err("must hit".into()))
            .unwrap()
            .1
            .len(),
        30
    );
}

#[test]
fn preexisting_over_budget_cache_cannot_succeed_when_eviction_fails() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("legacy.img"), vec![42; 80]).unwrap();
    let cache = ImageCache::with_remover(
        tmp.path().into(),
        50,
        40,
        Arc::new(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "eviction blocked",
            ))
        }),
    );
    assert!(
        cache
            .get("new", || image(10))
            .unwrap_err()
            .contains("eviction blocked")
    );
    assert_eq!(disk_bytes(tmp.path()), 80, "no additional bytes written");
}

#[test]
fn oversized_legacy_entry_is_evicted_before_serving() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(format!("{}.img", super::cache_key("old")));
    std::fs::write(&path, [b"image/png\n".as_slice(), &[42; 41]].concat()).unwrap();
    let cache = ImageCache::new(tmp.path().into(), 100, 40);
    assert_eq!(cache.get("old", || image(20)).unwrap().1.len(), 20);
    assert_eq!(disk_bytes(tmp.path()), 30);
}

#[test]
fn upstream_body_reader_enforces_limit_without_content_length() {
    let body = ureq::Body::builder().reader(std::io::Cursor::new(vec![42; 41]));
    assert!(super::read_image_body(body, 40).is_err());
    let body = ureq::Body::builder().reader(std::io::Cursor::new(vec![42; 40]));
    assert_eq!(super::read_image_body(body, 40).unwrap().len(), 40);
}

#[tokio::test]
async fn fast_cold_fetch_is_returned_without_a_pending_response() {
    use axum::http::{HeaderMap, StatusCode};
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let proxy = Arc::new(super::runtime::ImageProxy::new(
        Arc::new(ImageCache::new(tmp.path().into(), 1000, 100)),
        Arc::new(tokio::sync::Semaphore::new(1)),
        Arc::new(|_| image(20)),
    ));
    let response = proxy
        .response(
            super::ProxyQuery {
                url: "https://image.tmdb.org/fast-cold-fetch.jpg".into(),
            },
            HeaderMap::new(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 100)
            .await
            .unwrap()
            .len(),
        20
    );
}

#[tokio::test]
async fn saturated_proxy_returns_unavailable_without_fetching() {
    let gate = Arc::new(tokio::sync::Semaphore::new(1));
    let _permit = gate.clone().acquire_owned().await.unwrap();
    let response = super::proxy_with_gate(
        super::ProxyQuery {
            url: "https://image.tmdb.org/test.png".into(),
        },
        gate,
    )
    .await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn uncached_proxy_does_not_wait_for_upstream_capacity() {
    use axum::http::StatusCode;
    let response = super::proxy_with_gate(
        super::ProxyQuery {
            url: "https://image.tmdb.org/t/p/w342/not-cached-regression.jpg".into(),
        },
        Arc::new(tokio::sync::Semaphore::new(0)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], "image.pending");
}

#[tokio::test]
async fn proxy_miss_returns_before_fetch_finishes_and_then_serves_conditional_cache_hits() {
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use std::time::Duration;
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let cache = Arc::new(ImageCache::new(tmp.path().into(), 1000, 100));
    let gate = Arc::new(tokio::sync::Semaphore::new(1));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = parking_lot::Mutex::new(Some(started_tx));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = parking_lot::Mutex::new(release_rx);
    let proxy = Arc::new(super::runtime::ImageProxy::new(
        cache.clone(),
        gate.clone(),
        Arc::new(move |_| {
            if let Some(tx) = started_tx.lock().take() {
                let _ = tx.send(());
            }
            release_rx
                .lock()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            image(20)
        }),
    ));
    let url = "https://image.tmdb.org/cache-regression.jpg";
    let query = || super::ProxyQuery { url: url.into() };
    let response = tokio::time::timeout(
        Duration::from_millis(300),
        proxy.response(query(), HeaderMap::new()),
    )
    .await;
    release_tx.send(()).unwrap();
    assert_eq!(
        response
            .expect("miss must free the HTTP connection before remote completion")
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    started_rx.await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while cache.lookup(url).unwrap().is_none() || gate.available_permits() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let _upstream_busy = gate.acquire().await.unwrap();
    let response = proxy.response(query(), HeaderMap::new()).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "cache hits bypass upstream capacity"
    );
    assert_eq!(response.headers()["cache-control"], "public, max-age=86400");
    let etag = response.headers()["etag"].clone();
    let mut headers = HeaderMap::new();
    headers.insert(
        "if-none-match",
        HeaderValue::from_bytes(etag.as_bytes()).unwrap(),
    );
    let response = proxy.response(query(), headers).await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn saturated_proxy_queues_a_unique_miss_until_upstream_capacity_returns() {
    use axum::http::{HeaderMap, StatusCode};
    use std::time::Duration;
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let cache = Arc::new(ImageCache::new(tmp.path().into(), 1000, 100));
    let gate = Arc::new(tokio::sync::Semaphore::new(1));
    let busy = gate.clone().acquire_owned().await.unwrap();
    let proxy = Arc::new(super::runtime::ImageProxy::new(
        cache.clone(),
        gate,
        Arc::new(|_| image(20)),
    ));
    let url = "https://image.tmdb.org/queued-regression.jpg";
    let response = proxy
        .response(super::ProxyQuery { url: url.into() }, HeaderMap::new())
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(cache.lookup(url).unwrap().is_none());
    drop(busy);
    tokio::time::timeout(Duration::from_millis(300), async {
        while cache.lookup(url).unwrap().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a queued image must warm without another browser request");
}

#[test]
fn lookup_does_not_unlink_an_invalid_entry_outside_publication() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let cache = ImageCache::new(tmp.path().into(), 1000, 100);
    let path = tmp
        .path()
        .join(format!("{}.img", super::cache_key("invalid")));
    std::fs::write(&path, b"incomplete cache entry").unwrap();
    assert!(cache.lookup("invalid").unwrap().is_none());
    assert!(
        path.is_file(),
        "only serialized publication may discard invalid entries"
    );
    cache.get("invalid", || image(20)).unwrap();
    assert_eq!(cache.lookup("invalid").unwrap().unwrap().1, vec![42; 20]);
}

#[test]
fn stalled_cache_lookup_has_a_bounded_response_deadline() {
    use axum::http::{HeaderMap, StatusCode};
    use std::time::Duration;
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let cache = Arc::new(ImageCache::new(tmp.path().into(), 1000, 100));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let proxy = Arc::new(super::runtime::ImageProxy::new(
            cache,
            gate.clone(),
            Arc::new(|_| Err("offline".into())),
        ));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        });
        entered_rx.await.unwrap();
        let response = tokio::time::timeout(
            Duration::from_millis(1200),
            proxy.response(
                super::ProxyQuery {
                    url: "https://image.tmdb.org/stalled.jpg".into(),
                },
                HeaderMap::new(),
            ),
        )
        .await;
        gate.close();
        release_tx.send(()).unwrap();
        worker.await.unwrap();
        assert_eq!(
            response
                .expect("cache worker queueing must not retain a browser connection indefinitely")
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    });
}
