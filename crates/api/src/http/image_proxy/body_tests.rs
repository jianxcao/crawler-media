//! Exercise body transport failures through terminal HTTP delivery, without sockets.
use super::super::{ProxyQuery, cache::ImageCache, read_image_body, runtime::ImageProxy};
use axum::body::to_bytes;
use axum::http::{HeaderMap, StatusCode};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

#[derive(Clone, Copy)]
enum BodyFailure {
    Timeout,
    WrappedTimeout,
    Disconnected,
}
struct FiniteBody {
    emitted: bool,
    failure: BodyFailure,
}
impl std::io::Read for FiniteBody {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if !self.emitted {
            self.emitted = true;
            bytes[0] = 0x89;
            return Ok(1);
        }
        // Both forms occur at a body reader: socket IO may report TimedOut,
        // while ureq wraps its own deadline as an io::ErrorKind::Other.
        Err(match self.failure {
            BodyFailure::Timeout => {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "finite body timeout")
            }
            BodyFailure::WrappedTimeout => ureq::Error::Timeout(ureq::Timeout::RecvBody).into_io(),
            BodyFailure::Disconnected => std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "finite body disconnect",
            ),
        })
    }
}
async fn assert_terminal_body_error(failure: BodyFailure, expected: StatusCode, code: &str) {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let cache = Arc::new(ImageCache::new(temp.path().into(), 1000, 100));
    let gate = Arc::new(Semaphore::new(1));
    let proxy = Arc::new(ImageProxy::new(
        cache.clone(),
        gate.clone(),
        Arc::new(move |_| {
            let body = ureq::Body::builder().reader(FiniteBody {
                emitted: false,
                failure,
            });
            let bytes = read_image_body(body, 100)?;
            Ok(("image/png".into(), bytes))
        }),
    ));
    let url = "https://image.tmdb.org/finite-body-failure.png";
    let response = tokio::time::timeout(
        Duration::from_millis(500),
        proxy.response(ProxyQuery { url: url.into() }, HeaderMap::new()),
    )
    .await
    .expect("finite body failure must complete before the observer deadline");
    tokio::time::timeout(Duration::from_secs(1), async {
        while gate.available_permits() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("body failure must release the actual worker permit");
    assert!(
        cache.lookup(url).unwrap().is_none(),
        "partial body was cached"
    );
    assert_eq!(response.status(), expected);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("retry-after"));
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["error"]["code"], code);
}

#[tokio::test]
async fn proxy_body_timeout_remains_gateway_timeout_without_caching_partial_bytes() {
    for failure in [BodyFailure::Timeout, BodyFailure::WrappedTimeout] {
        assert_terminal_body_error(failure, StatusCode::GATEWAY_TIMEOUT, "image.timeout").await;
    }
}

#[tokio::test]
async fn proxy_body_disconnect_remains_bad_gateway_without_caching_partial_bytes() {
    assert_terminal_body_error(
        BodyFailure::Disconnected,
        StatusCode::BAD_GATEWAY,
        "image.upstream",
    )
    .await;
}
