use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use domain::MediaId;
use parking_lot::Mutex;
use tokio::sync::Semaphore;

use crate::catalog::Catalog;
use crate::Store;
use super::cache::{DiskPosterCache, NEGATIVE_TTL};
use super::http::{ImageFetcher, RestricedImageFetcher};
use super::{MediaPosterSource, PosterBytes, PosterError};

pub struct MediaPosterService {
    store: Arc<Mutex<Store>>,
    catalog: Arc<dyn Catalog>,
    cache: DiskPosterCache,
    fetcher: Arc<dyn ImageFetcher>,
    in_flight: Arc<Mutex<HashMap<MediaId, Arc<tokio::sync::Mutex<()>>>>>,
    negative_cache: Arc<Mutex<HashMap<MediaId, (Instant, PosterError)>>>,
    global_concurrency: Arc<Semaphore>,
}

impl MediaPosterService {
    pub fn new(store: Arc<Mutex<Store>>, catalog: Arc<dyn Catalog>) -> Self {
        Self {
            store,
            catalog,
            cache: DiskPosterCache::new(DiskPosterCache::default_dir()),
            fetcher: Arc::new(RestricedImageFetcher),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            negative_cache: Arc::new(Mutex::new(HashMap::new())),
            global_concurrency: Arc::new(Semaphore::new(4)),
        }
    }

    pub fn with_custom_fetcher(
        store: Arc<Mutex<Store>>,
        catalog: Arc<dyn Catalog>,
        cache_dir: std::path::PathBuf,
        fetcher: Arc<dyn ImageFetcher>,
    ) -> Self {
        Self {
            store,
            catalog,
            cache: DiskPosterCache::new(cache_dir),
            fetcher,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            negative_cache: Arc::new(Mutex::new(HashMap::new())),
            global_concurrency: Arc::new(Semaphore::new(4)),
        }
    }

    fn check_negative_cache(&self, media_id: MediaId) -> Option<PosterError> {
        let mut neg = self.negative_cache.lock();
        if let Some((inserted, err)) = neg.get(&media_id) {
            if inserted.elapsed() < NEGATIVE_TTL {
                return Some(err.clone());
            }
        }
        neg.remove(&media_id);
        None
    }

    fn record_negative_cache(&self, media_id: MediaId, err: PosterError) {
        let should_cache = matches!(err, PosterError::NotFound | PosterError::Upstream(_) | PosterError::Timeout);
        if should_cache {
            self.negative_cache.lock().insert(media_id, (Instant::now(), err));
        }
    }
}

#[async_trait::async_trait]
impl MediaPosterSource for MediaPosterService {
    async fn get(&self, media_id: MediaId) -> Result<PosterBytes, PosterError> {
        if let Some(err) = self.check_negative_cache(media_id) {
            return Err(err);
        }

        // Fast path: memory/disk cache without lock contention
        let cache_clone = self.cache.clone();
        if let Some(hit) = tokio::task::spawn_blocking(move || cache_clone.get(media_id))
            .await
            .ok()
            .flatten()
        {
            return Ok(hit);
        }

        // Per-media single-flight coordination
        let gate = {
            let mut map = self.in_flight.lock();
            map.entry(media_id)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };

        let _guard = gate.lock().await;

        // Double check cache
        let cache_clone = self.cache.clone();
        if let Some(hit) = tokio::task::spawn_blocking(move || cache_clone.get(media_id))
            .await
            .ok()
            .flatten()
        {
            return Ok(hit);
        }

        // Global concurrency bound (max 4 upstream requests)
        let _permit = self
            .global_concurrency
            .acquire()
            .await
            .map_err(|e| PosterError::Io(e.to_string()))?;

        let store = self.store.clone();
        let catalog = self.catalog.clone();
        let fetcher = self.fetcher.clone();
        let cache = self.cache.clone();

        let fetch_future = tokio::task::spawn_blocking(move || -> Result<PosterBytes, PosterError> {
            let media = {
                let s = store.lock();
                s.get_media(media_id)
                    .map_err(|e| PosterError::Io(e.to_string()))?
            };
            let media = media.ok_or(PosterError::NotFound)?;
            let tmdb_id = media.tmdb_id.as_deref().ok_or(PosterError::NotFound)?;

            let url = catalog
                .poster_url(media.kind, tmdb_id)
                .map_err(PosterError::Upstream)?
                .ok_or(PosterError::NotFound)?;

            let (content_type, bytes) = fetcher.fetch(&url)?;
            cache.put(media_id, &content_type, &bytes)?;

            Ok(PosterBytes {
                content_type,
                bytes: Arc::from(bytes.into_boxed_slice()),
            })
        });

        let result = match tokio::time::timeout(Duration::from_secs(10), fetch_future).await {
            Ok(Ok(inner_res)) => inner_res,
            Ok(Err(join_err)) => Err(PosterError::Io(join_err.to_string())),
            Err(_) => Err(PosterError::Timeout),
        };

        {
            let mut map = self.in_flight.lock();
            map.remove(&media_id);
        }

        match result {
            Ok(bytes) => Ok(bytes),
            Err(err) => {
                self.record_negative_cache(media_id, err.clone());
                Err(err)
            }
        }
    }
}
