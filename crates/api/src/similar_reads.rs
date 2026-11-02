//! Bounded background refresh for optional recommendations. Successful bodies
//! live only in the catalog's durable cache, never in a second in-memory cache.
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use domain::MediaKind;
use parking_lot::Mutex;
use tokio::sync::Semaphore;

use crate::catalog::Catalog;

const CAPACITY: usize = 64;
const WORKERS: usize = 4;
const COMPLETED_TTL: Duration = Duration::from_secs(5);
type Key = (MediaKind, String);

enum Entry {
    Pending,
    Completed {
        at: Instant,
        result: Result<(), String>,
    },
}
pub(crate) enum SimilarRead {
    Ready(Vec<media::CatalogHit>),
    Pending,
    Failed(String),
    Busy,
}
pub(crate) struct SimilarReads {
    entries: Mutex<HashMap<Key, Entry>>,
    workers: Arc<Semaphore>,
}
impl Default for SimilarReads {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            workers: Arc::new(Semaphore::new(WORKERS)),
        }
    }
}
impl SimilarReads {
    pub(crate) fn read(
        self: &Arc<Self>,
        catalog: Arc<dyn Catalog>,
        kind: MediaKind,
        id: String,
    ) -> SimilarRead {
        // Cache IO precedes the registry lock. Ready/NotFound survives restarts.
        match catalog.cached_similar(kind, &id) {
            Ok(Some(hits)) => return SimilarRead::Ready(hits),
            Err(error) => return SimilarRead::Failed(error),
            Ok(None) => {}
        }
        let key = (kind, id.clone());
        {
            let mut entries = self.entries.lock();
            entries.retain(|_, entry| match entry {
                Entry::Pending => true,
                Entry::Completed { at, .. } => at.elapsed() < COMPLETED_TTL,
            });
            match entries.get(&key) {
                Some(Entry::Pending) => return SimilarRead::Pending,
                Some(Entry::Completed {
                    result: Err(error), ..
                }) => return SimilarRead::Failed(error.clone()),
                Some(Entry::Completed { result: Ok(()), .. }) => {
                    // Completion may have raced the first cache observation.
                    // Re-read after seeing the completion marker, without a lock.
                    drop(entries);
                    return match catalog.cached_similar(kind, &id) {
                        Ok(Some(hits)) => SimilarRead::Ready(hits),
                        Err(error) => SimilarRead::Failed(error),
                        Ok(None) => SimilarRead::Failed(
                            "catalog did not persist the completed recommendation response".into(),
                        ),
                    };
                }
                None => {}
            }
            if entries.len() == CAPACITY {
                // Completed observations may be discarded; queued/running work
                // must never be evicted and duplicated while still in flight.
                let completed = entries
                    .iter()
                    .filter_map(|(key, entry)| match entry {
                        Entry::Completed { at, .. } => Some((key.clone(), *at)),
                        Entry::Pending => None,
                    })
                    .min_by_key(|(_, at)| *at)
                    .map(|(key, _)| key);
                if let Some(key) = completed {
                    entries.remove(&key);
                } else {
                    return SimilarRead::Busy;
                }
            }
            entries.insert(key.clone(), Entry::Pending);
        }
        let owner = self.clone();
        tokio::spawn(async move {
            // At most 64 detached waiters and four actual origin workers. The
            // permit lives in the real blocking operation, beyond HTTP cancel.
            let result = match owner.workers.clone().acquire_owned().await {
                Ok(permit) => tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    catalog.similar(kind, &id).map(|_| ())
                })
                .await
                .map_err(|error| format!("recommendation worker failed: {error}"))
                .and_then(|result| result),
                Err(_) => Err("recommendation refresh admission closed".into()),
            };
            let result = result.map_err(|error| error.chars().take(512).collect());
            owner.entries.lock().insert(
                key,
                Entry::Completed {
                    at: Instant::now(),
                    result,
                },
            );
        });
        SimilarRead::Pending
    }
}
