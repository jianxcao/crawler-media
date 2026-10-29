//! Cache hits bypass the shared upstream window and consume the same absolute budget.
use super::*;

impl MediaPosterService {
    pub(super) async fn cached_bytes(
        &self,
        media_id: MediaId,
        deadline: Instant,
    ) -> Result<Option<PosterBytes>, PosterError> {
        let remaining = || deadline.saturating_duration_since(Instant::now());
        if remaining().is_zero() {
            return Err(PosterError::Timeout);
        }
        let cache = self.cache.clone();
        let cached = self
            .cache_reads
            .reserve(remaining().min(Duration::from_millis(250)))
            .await
            .map_err(|_| {
                if Instant::now() >= deadline {
                    PosterError::Timeout
                } else {
                    PosterError::Busy
                }
            })?
            .run(remaining().min(Duration::from_millis(750)), move || {
                cache.get(media_id)
            })
            .await
            .map_err(|error| match error {
                crate::blocking_budget::BlockingError::Busy => PosterError::Busy,
                crate::blocking_budget::BlockingError::Timeout => PosterError::Timeout,
                error => PosterError::Io(error.to_string()),
            })?;
        if Instant::now() >= deadline {
            return Err(PosterError::Timeout);
        }
        if cached.is_some() {
            return Ok(cached);
        }
        match self.check_negative_cache(media_id) {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }
}
