//! Local HTTP reads and bounded admission for the existing background worker.
use super::*;

const WARM_CAPACITY: usize = 32;

impl MediaPosterService {
    pub(super) async fn cached_bytes(
        &self,
        media_id: MediaId,
    ) -> Result<Option<PosterBytes>, PosterError> {
        let cache = self.cache.clone();
        let cached = self
            .cache_reads
            .reserve(Duration::from_millis(250))
            .await
            .map_err(|_| PosterError::Busy)?
            .run(Duration::from_millis(750), move || cache.get(media_id))
            .await
            .map_err(|error| match error {
                crate::blocking_budget::BlockingError::Busy => PosterError::Busy,
                crate::blocking_budget::BlockingError::Timeout => PosterError::Timeout,
                error => PosterError::Io(error.to_string()),
            })?;
        if cached.is_some() {
            return Ok(cached);
        }
        match self.check_negative_cache(media_id) {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    pub(super) fn start_warm(self: Arc<Self>, media_id: MediaId) -> Result<(), PosterError> {
        {
            let mut warming = self.warming.lock();
            let mut running = self.in_flight.lock();
            running.retain(|_, entry| entry.upgrade().is_some());
            if warming.contains(&media_id) || running.contains_key(&media_id) {
                return Ok(());
            }
            // `get` can time out while its real worker remains queued/running.
            // Count both sets so caller timeouts never reopen admission early.
            let admitted: HashSet<_> = warming.iter().chain(running.keys()).copied().collect();
            if admitted.len() >= WARM_CAPACITY {
                tracing::warn!(%media_id, "Media poster warmup queue full");
                return Err(PosterError::Busy);
            }
            warming.insert(media_id);
        }
        let flight = WarmFlight {
            warming: self.warming.clone(),
            media_id,
        };
        tokio::spawn(async move {
            let _flight = flight;
            if let Err(error) = self.get(media_id).await {
                tracing::warn!(%media_id, %error, "Background Media poster delivery did not complete for its observer");
            }
        });
        Ok(())
    }
}

struct WarmFlight {
    warming: Arc<Mutex<HashSet<MediaId>>>,
    media_id: MediaId,
}

impl Drop for WarmFlight {
    fn drop(&mut self) {
        self.warming.lock().remove(&self.media_id);
    }
}
