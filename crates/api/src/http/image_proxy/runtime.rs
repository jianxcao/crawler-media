//! One bounded shared fill per URL; observers receive bytes or a terminal error.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use parking_lot::Mutex;
use tokio::sync::Semaphore;

use super::cache::{Image, ImageCache};
use super::phase::{self, Progress};
use super::{ProxyQuery, allowed_url};
use crate::http::err;
use crate::http::library_artwork::image_response::conditional_image;

mod fill;
pub(super) use fill::DeliveryError;
use fill::{Fill, WaitResult};
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
        let mut observer = Observer::new();
        let result = self.deliver(query.url, deadline, &mut observer).await;
        if matches!(result, Err(DeliveryError::Timeout)) {
            observer.timeout();
        }
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
        observer: &mut Observer,
    ) -> Result<Arc<Image>, DeliveryError> {
        let cache = self.cache.clone();
        let lookup_url = url.clone();
        let remaining = || deadline.saturating_duration_since(Instant::now());
        observer.progress.set("read_admission");
        let reservation = self
            .reads
            .reserve(remaining().min(Duration::from_millis(250)))
            .await
            .map_err(|error| {
                observer.timeout_source = "read_admission";
                observer.observer_deadline_expired = Instant::now() >= deadline;
                if observer.observer_deadline_expired {
                    DeliveryError::Timeout
                } else {
                    blocking_error(error)
                }
            })?;
        observer.progress.set("read_worker_queue");
        let progress = observer.progress.clone();
        let lookup = reservation
            .run(remaining().min(Duration::from_millis(750)), move || {
                progress.set("cache_lookup");
                cache.lookup(&lookup_url)
            })
            .await
            .map_err(|error| {
                observer.timeout_source = "cache_lookup";
                observer.observer_deadline_expired = Instant::now() >= deadline;
                blocking_error(error)
            })?
            .map_err(DeliveryError::Cache)?;
        if Instant::now() >= deadline {
            observer.timeout_source = "observer_deadline";
            observer.observer_deadline_expired = true;
            return Err(DeliveryError::Timeout);
        }
        if let Some(image) = lookup {
            return Ok(Arc::new(image));
        }
        let (fill, role) = self.start_fill(url, deadline)?;
        observer.progress = fill.progress.clone();
        observer.fill = Some((fill.clone(), role));
        match fill.wait(deadline).await {
            WaitResult::Shared(outcome) => {
                observer.timeout_source = "shared_fill_result";
                observer.observer_deadline_expired = Instant::now() >= deadline;
                outcome
            }
            WaitResult::Deadline => {
                observer.timeout_source = "observer_deadline";
                observer.observer_deadline_expired = true;
                Err(DeliveryError::Timeout)
            }
        }
    }
}

struct Observer {
    id: u64,
    started: Instant,
    progress: Arc<Progress>,
    fill: Option<(Arc<Fill>, &'static str)>,
    timeout_source: &'static str,
    observer_deadline_expired: bool,
}
impl Observer {
    fn new() -> Self {
        Self {
            id: phase::id(),
            started: Instant::now(),
            progress: Progress::new("read_admission"),
            fill: None,
            timeout_source: "observer_deadline",
            observer_deadline_expired: false,
        }
    }
    fn timeout(&self) {
        let (phase, phase_elapsed_ms) = self.progress.snapshot();
        let (fill_id, role) = self
            .fill
            .as_ref()
            .map_or((0, "none"), |(fill, role)| (fill.id, *role));
        tracing::warn!(target: "api::image_delivery", event = "image_delivery_timeout", observer_id = self.id, fill_id, role, phase, timeout_source = self.timeout_source, observer_deadline_expired = self.observer_deadline_expired, observer_elapsed_ms = phase::millis(self.started), phase_elapsed_ms);
        if let Some((fill, _)) = &self.fill {
            fill.observer_timed_out();
        }
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
    tracing::warn!(class = error.class(), "Image delivery failed");
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
