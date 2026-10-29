//! Queue expiry cannot start upstream work; active synchronous work owns its permits.
use super::*;

struct ActiveWorker(Arc<AtomicUsize>);
impl ActiveWorker {
    fn start(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}
impl Drop for ActiveWorker {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl MediaPosterService {
    pub(super) fn spawn_worker(&self, media_id: MediaId, entry: Arc<PosterInflight>) {
        let readers = self.readers.clone();
        let catalog = self.catalog.clone();
        let fetcher = self.fetcher.clone();
        let cache = self.cache.clone();
        let sem = self.global_concurrency.clone();
        let in_flight = self.in_flight.clone();
        let negative_cache = self.negative_cache.clone();
        let active_workers = self.active_workers.clone();
        let deadline = entry.deadline;
        let mut guard = PosterWorkerGuard::new(media_id, entry, in_flight);
        tokio::spawn(async move {
            let permit = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                sem.acquire_owned(),
            )
            .await;
            let permit = match permit {
                Ok(Ok(permit)) => permit,
                Ok(Err(_)) => {
                    guard.complete(Err(PosterError::Io("concurrency semaphore closed".into())));
                    return;
                }
                Err(_) => {
                    guard.complete(Err(PosterError::Timeout));
                    return;
                }
            };
            guard.set_permit(permit);
            let _ = tokio::task::spawn_blocking(move || {
                let _active = ActiveWorker::start(active_workers);
                let outcome =
                    fetch_poster(&readers, &*catalog, &*fetcher, &cache, media_id, deadline);
                if let Err(error) = &outcome {
                    Self::record_negative_cache(&negative_cache, media_id, error);
                }
                guard.complete(outcome);
            })
            .await;
        });
    }
}

fn fetch_poster(
    readers: &Arc<crate::store_readers::StoreReaders>,
    catalog: &dyn Catalog,
    fetcher: &dyn ImageFetcher,
    cache: &DiskPosterCache,
    media_id: MediaId,
    deadline: Instant,
) -> Result<PosterBytes, PosterError> {
    check_deadline(deadline)?;
    if let Some(hit) = cache.get(media_id) {
        return Ok(hit);
    }
    let (kind, tmdb_id) = {
        let wait = deadline.saturating_duration_since(Instant::now());
        let store = readers.checkout(wait).ok_or(PosterError::Timeout)?;
        check_deadline(deadline)?;
        let media = store
            .get_media(media_id)
            .map_err(|error| PosterError::Io(error.to_string()))?
            .ok_or(PosterError::NotFound)?;
        (media.kind, media.tmdb_id.ok_or(PosterError::NotFound)?)
    };
    check_deadline(deadline)?;
    let url = catalog
        .poster_url(kind, &tmdb_id)
        .map_err(PosterError::Upstream)?
        .ok_or(PosterError::NotFound)?;
    check_deadline(deadline)?;
    // An already running synchronous source cannot be forcibly aborted. Its real
    // worker retains the permit/flight until it returns, including observer timeout.
    let (content_type, bytes) = fetcher.fetch(&url)?;
    cache.put(media_id, &content_type, &bytes)?;
    Ok(PosterBytes {
        content_type,
        bytes: Arc::from(bytes.into_boxed_slice()),
    })
}
fn check_deadline(deadline: Instant) -> Result<(), PosterError> {
    if Instant::now() >= deadline {
        Err(PosterError::Timeout)
    } else {
        Ok(())
    }
}
