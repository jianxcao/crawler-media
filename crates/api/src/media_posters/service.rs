use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use domain::MediaId;
use parking_lot::Mutex;
use tokio::sync::Semaphore;

use super::cache::{DiskPosterCache, NEGATIVE_TTL};
use super::http::{ImageFetcher, RestricedImageFetcher};
use super::inflight::{InflightMap, PosterInflight, PosterWorkerGuard, wait_poster};
use super::negative_cache::{NegativePosterCache, PosterClock, SystemPosterClock};
use super::{MediaPosterSource, PosterBytes, PosterError, PosterResourceCounts};
use crate::Store;
use crate::catalog::Catalog;

const NEGATIVE_CACHE_CAPACITY: usize = 4_096;
const GATE_REGISTRY_CAPACITY: usize = 32;

mod delivery;
mod worker;

pub struct MediaPosterService {
    readers: Arc<crate::store_readers::StoreReaders>,
    catalog: Arc<dyn Catalog>,
    cache: DiskPosterCache,
    fetcher: Arc<dyn ImageFetcher>,
    in_flight: Arc<Mutex<InflightMap>>,
    negative_cache: Arc<Mutex<NegativePosterCache>>,
    global_concurrency: Arc<Semaphore>,
    active_workers: Arc<AtomicUsize>,
    wait_deadline: Duration,
    cache_reads: crate::blocking_budget::BlockingBudget,
}

impl MediaPosterService {
    pub(crate) fn new(
        readers: Arc<crate::store_readers::StoreReaders>,
        catalog: Arc<dyn Catalog>,
    ) -> Self {
        Self::configured(
            readers,
            catalog,
            DiskPosterCache::default_dir(),
            Arc::new(RestricedImageFetcher),
        )
    }

    pub fn with_custom_fetcher(
        store: Arc<Mutex<Store>>,
        catalog: Arc<dyn Catalog>,
        cache_dir: std::path::PathBuf,
        fetcher: Arc<dyn ImageFetcher>,
    ) -> Result<Self, store::StoreError> {
        let readers = Arc::new(crate::store_readers::StoreReaders::open(&store.lock(), 4)?);
        Ok(Self::configured(readers, catalog, cache_dir, fetcher))
    }

    fn configured(
        readers: Arc<crate::store_readers::StoreReaders>,
        catalog: Arc<dyn Catalog>,
        cache_dir: std::path::PathBuf,
        fetcher: Arc<dyn ImageFetcher>,
    ) -> Self {
        let clock = Arc::new(SystemPosterClock::new());
        Self {
            readers,
            catalog,
            cache: DiskPosterCache::new(cache_dir),
            fetcher,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            negative_cache: Arc::new(Mutex::new(NegativePosterCache::new(
                NEGATIVE_CACHE_CAPACITY,
                NEGATIVE_TTL,
                clock,
            ))),
            global_concurrency: Arc::new(Semaphore::new(4)),
            active_workers: Arc::new(AtomicUsize::new(0)),
            wait_deadline: Duration::from_secs(2),
            cache_reads: crate::blocking_budget::BlockingBudget::new(4),
        }
    }

    pub fn with_wait_deadline(mut self, deadline: Duration) -> Self {
        self.wait_deadline = deadline;
        self
    }
    pub fn with_negative_cache_clock(mut self, clock: Arc<dyn PosterClock>) -> Self {
        self.negative_cache = Arc::new(Mutex::new(NegativePosterCache::new(
            NEGATIVE_CACHE_CAPACITY,
            NEGATIVE_TTL,
            clock,
        )));
        self
    }

    pub fn resource_counts(&self) -> PosterResourceCounts {
        let in_flight = self.in_flight.lock();
        let registry_entries = in_flight.len();
        drop(in_flight);
        let negative_entries = self.negative_cache.lock().len();
        let active_workers = self.active_workers.load(Ordering::SeqCst);

        PosterResourceCounts {
            negative_entries,
            registry_entries,
            active_workers,
        }
    }

    /// Diagnostic probe returning the number of in-flight entries.
    /// Not called by production code.
    #[doc(hidden)]
    pub fn inflight_entries(&self) -> usize {
        self.in_flight.lock().len()
    }

    /// Diagnostic probe returning the number of active waiters for a media item.
    /// Returns 0 when no entry exists. Not called by production code.
    #[doc(hidden)]
    pub fn inflight_waiters(&self, media_id: MediaId) -> usize {
        self.in_flight
            .lock()
            .get(&media_id)
            .and_then(|weak| weak.upgrade())
            .map_or(0, |entry| entry.waiter_count())
    }

    /// Test helper to insert a weak inflight gate without exposing the internal map.
    #[doc(hidden)]
    pub fn insert_inflight_for_test(&self, media_id: MediaId, entry: &Arc<PosterInflight>) {
        self.in_flight
            .lock()
            .insert(media_id, Arc::downgrade(entry));
    }

    /// Test helper to check if a key is present in the negative cache.
    #[doc(hidden)]
    pub fn check_negative_cache_for_test(&self, media_id: MediaId) -> bool {
        self.negative_cache.lock().get(media_id).is_some()
    }

    fn check_negative_cache(&self, media_id: MediaId) -> Option<PosterError> {
        self.negative_cache.lock().get(media_id)
    }

    fn record_negative_cache(
        negative_cache: &Mutex<NegativePosterCache>,
        media_id: MediaId,
        err: &PosterError,
    ) {
        let should_cache = matches!(
            err,
            PosterError::NotFound | PosterError::Upstream(_) | PosterError::Timeout
        );
        if should_cache {
            negative_cache.lock().insert(media_id, err.clone());
        }
    }
}

#[async_trait::async_trait]
impl MediaPosterSource for MediaPosterService {
    async fn cached(&self, media_id: MediaId) -> Result<Option<PosterBytes>, PosterError> {
        self.cached_bytes(media_id, Instant::now() + self.wait_deadline)
            .await
    }

    async fn get(&self, media_id: MediaId) -> Result<PosterBytes, PosterError> {
        let deadline = Instant::now() + self.wait_deadline;
        if let Some(poster) = self.cached_bytes(media_id, deadline).await? {
            return Ok(poster);
        }
        if Instant::now() >= deadline {
            return Err(PosterError::Timeout);
        }

        let (entry, is_leader) = {
            let mut map = self.in_flight.lock();
            // Clean dead entries before checking capacity / admitting new entry
            map.retain(|_, weak| weak.upgrade().is_some());

            if let Some(existing) = map.get(&media_id).and_then(std::sync::Weak::upgrade) {
                (existing, false)
            } else {
                if map.len() >= GATE_REGISTRY_CAPACITY {
                    return Err(PosterError::Busy);
                }
                let entry = Arc::new(PosterInflight::new(deadline));
                map.insert(media_id, Arc::downgrade(&entry));
                (entry, true)
            }
        };

        if is_leader {
            self.spawn_worker(media_id, entry.clone());
        }
        let res = wait_poster(entry, deadline).await;

        match res {
            Ok(bytes) => {
                tracing::info!(media_id = %media_id, "海报获取成功");
                Ok(bytes)
            }
            Err(err) => {
                let outcome = match &err {
                    PosterError::NotFound => "not_found",
                    PosterError::Timeout => "timeout",
                    PosterError::InvalidImage => "invalid_image",
                    PosterError::Upstream(_) => "upstream",
                    PosterError::Io(_) => "io",
                    PosterError::Busy => "busy",
                };
                if matches!(err, PosterError::Timeout) {
                    tracing::warn!(media_id = %media_id, outcome, "海报等待已到截止时间；真实 worker 结束前保留其资源");
                } else {
                    tracing::error!(media_id = %media_id, outcome, "海报获取失败");
                }
                Err(err)
            }
        }
    }
}
