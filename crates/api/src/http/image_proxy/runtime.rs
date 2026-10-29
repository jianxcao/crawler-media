//! Cache reads never queue behind an upstream fetch. Misses warm the cache in
//! bounded, deduplicated background work and immediately free the HTTP connection.
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use parking_lot::Mutex;
use tokio::sync::Semaphore;

use super::cache::{Image, ImageCache};
use super::{ProxyQuery, allowed_url};
use crate::http::err;
use crate::http::library_artwork::image_response::conditional_image;

type FetchImage = Arc<dyn Fn(&str) -> Result<Image, String> + Send + Sync>;

pub(super) struct ImageProxy {
    cache: Arc<ImageCache>,
    reads: crate::blocking_budget::BlockingBudget,
    gate: Arc<Semaphore>,
    pending: Mutex<HashSet<String>>,
    fetch: FetchImage,
}

impl ImageProxy {
    pub(super) fn new(cache: Arc<ImageCache>, gate: Arc<Semaphore>, fetch: FetchImage) -> Self {
        Self {
            cache,
            reads: crate::blocking_budget::BlockingBudget::new(8),
            gate,
            pending: Mutex::new(HashSet::new()),
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
        let cache = self.cache.clone();
        let url = query.url.clone();
        let lookup = async {
            self.reads
                .reserve(Duration::from_millis(250))
                .await?
                .run(Duration::from_millis(750), move || cache.lookup(&url))
                .await
        }
        .await;
        match lookup {
            Ok(Ok(Some((kind, bytes)))) => {
                let mut response = conditional_image(&headers, bytes, &kind, false);
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=86400"),
                );
                return response;
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => tracing::warn!(%error, "Image cache lookup failed"),
            Err(error) => tracing::error!(%error, "Image cache worker failed"),
        }
        self.start_fetch(query.url);
        let mut response = err(
            StatusCode::SERVICE_UNAVAILABLE,
            "image.pending",
            "图片正在缓存，请稍后重试",
        );
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }

    fn start_fetch(self: &Arc<Self>, url: String) {
        let mut pending = self.pending.lock();
        if pending.contains(&url) {
            return;
        }
        // Keep admission bounded even if a page contains many cold images. The
        // semaphore queue is fair; a browser retry never has to win a race for a
        // newly free upstream slot.
        if pending.len() >= 64 {
            tracing::warn!("Image cache warmup queue full; retry on next image request");
            return;
        }
        pending.insert(url.clone());
        let proxy = self.clone();
        tokio::spawn(async move {
            let flight = Flight {
                proxy: proxy.clone(),
                url: url.clone(),
            };
            let Ok(permit) = proxy.gate.clone().acquire_owned().await else {
                return;
            };
            let result = tokio::task::spawn_blocking(move || {
                let _flight = flight;
                let _permit = permit;
                match proxy.cache.get(&url, || (proxy.fetch)(&url)) {
                    Ok(_) => tracing::debug!(%url, "Image cache warmup finished"),
                    Err(error) => tracing::warn!(%error, %url, "Image cache warmup failed"),
                }
            })
            .await;
            if let Err(error) = result {
                tracing::error!(%error, "Image cache warmup worker failed");
            }
        });
    }
}

struct Flight {
    proxy: Arc<ImageProxy>,
    url: String,
}

impl Drop for Flight {
    fn drop(&mut self) {
        self.proxy.pending.lock().remove(&self.url);
    }
}
