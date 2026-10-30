//! One bounded shared fill per URL; observers receive bytes or a terminal error.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use parking_lot::Mutex;
use tokio::sync::Semaphore;

use super::cache::{Image, ImageCache};
use super::{ProxyQuery, allowed_url};
use crate::http::err;
use crate::http::library_artwork::image_response::conditional_image;

mod fill;
pub(super) use fill::DeliveryError;
use fill::Fill;
pub(super) type FetchImage = Arc<dyn Fn(&str) -> Result<Image, DeliveryError> + Send + Sync>;
const DELIVERY_DEADLINE: Duration = Duration::from_secs(2);

pub(crate) struct ImageProxy {
    cache: Arc<ImageCache>,
    reads: crate::blocking_budget::BlockingBudget,
    gate: Arc<Semaphore>,
    pending: Mutex<HashMap<String, Arc<Fill>>>,
    fetch: FetchImage,
}

impl ImageProxy {
    pub(crate) async fn response_for_url(
        self: &Arc<Self>,
        url: String,
        headers: HeaderMap,
    ) -> Response {
        self.response(ProxyQuery { url }, headers).await
    }

    pub(super) fn new(cache: Arc<ImageCache>, gate: Arc<Semaphore>, fetch: FetchImage) -> Self {
        Self {
            cache,
            reads: crate::blocking_budget::BlockingBudget::new(8),
            gate,
            pending: Mutex::new(HashMap::new()),
            fetch,
        }
    }

    pub(super) async fn response(
        self: &Arc<Self>,
        query: ProxyQuery,
        headers: HeaderMap,
    ) -> Response {
        if !allowed_url(&query.url) {
            return err(StatusCode::BAD_REQUEST, "image.invalid", "不允许的图床");
        }
        let deadline = Instant::now() + DELIVERY_DEADLINE;
        let result = self.deliver(query.url, deadline).await;
        match result {
            Ok(image) => {
                let mut response = conditional_image(&headers, image.1.clone(), &image.0, false);
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=86400"),
                );
                response
            }
            Err(error) => failure(error),
        }
    }

    async fn deliver(
        self: &Arc<Self>,
        url: String,
        deadline: Instant,
    ) -> Result<Arc<Image>, DeliveryError> {
        let cache = self.cache.clone();
        let lookup_url = url.clone();
        let remaining = || deadline.saturating_duration_since(Instant::now());
        let reservation = self
            .reads
            .reserve(remaining().min(Duration::from_millis(250)))
            .await
            .map_err(|error| {
                if Instant::now() >= deadline {
                    DeliveryError::Timeout
                } else {
                    blocking_error(error)
                }
            })?;
        let lookup = reservation
            .run(remaining().min(Duration::from_millis(750)), move || {
                cache.lookup(&lookup_url)
            })
            .await
            .map_err(blocking_error)?
            .map_err(DeliveryError::Cache)?;
        if Instant::now() >= deadline {
            return Err(DeliveryError::Timeout);
        }
        if let Some(image) = lookup {
            return Ok(Arc::new(image));
        }
        self.start_fill(url, deadline)?.wait(deadline).await
    }
}

fn blocking_error(error: crate::blocking_budget::BlockingError) -> DeliveryError {
    use crate::blocking_budget::BlockingError;
    match error {
        BlockingError::Busy => DeliveryError::Busy,
        BlockingError::Timeout => DeliveryError::Timeout,
        BlockingError::Worker(message) => DeliveryError::Cache(message),
    }
}
fn failure(error: DeliveryError) -> Response {
    tracing::warn!(%error, "Image delivery failed");
    let mut response = match error {
        DeliveryError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "image.busy",
            "图片服务繁忙",
        ),
        DeliveryError::Timeout => err(StatusCode::GATEWAY_TIMEOUT, "image.timeout", "图片请求超时"),
        DeliveryError::Upstream(_) => {
            err(StatusCode::BAD_GATEWAY, "image.upstream", "图片源请求失败")
        }
        DeliveryError::Cache(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "image.cache",
            "图片缓存读写失败",
        ),
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
