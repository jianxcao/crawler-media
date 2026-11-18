//! Instance-owned cloud lookup configuration and bounded negative results.
use crate::theintrodb::TheIntroDbClient;

pub trait ClientFactory: Send + Sync {
    fn create(&self, api_key: Option<String>) -> TheIntroDbClient;
}

pub(crate) struct HttpClientFactory;
impl ClientFactory for HttpClientFactory {
    fn create(&self, api_key: Option<String>) -> TheIntroDbClient {
        TheIntroDbClient::new(api_key)
    }
}

use crate::{ScrapeStoreExt, Store};
use domain::{FileVersion, LedgerId, UnitKey};
use parking_lot::Mutex;
use std::collections::HashMap;

#[derive(Clone, PartialEq, Eq, Hash)]
struct FlightTarget {
    file_id: LedgerId,
    version: FileVersion,
    unit: UnitKey,
    media_id: domain::MediaId,
    tmdb_id: String,
}

#[derive(Default)]
pub(crate) struct InFlight(Mutex<std::collections::HashSet<FlightTarget>>);

impl InFlight {
    fn insert(&self, target: FlightTarget) -> bool {
        self.0.lock().insert(target)
    }

    fn remove(&self, target: &FlightTarget) {
        self.0.lock().remove(target);
    }

    pub(crate) fn is_active(
        &self,
        file_id: LedgerId,
        version: &FileVersion,
        unit: UnitKey,
        media_id: domain::MediaId,
        tmdb_id: &str,
    ) -> bool {
        self.0.lock().contains(&FlightTarget {
            file_id,
            version: version.clone(),
            unit,
            media_id,
            tmdb_id: tmdb_id.to_string(),
        })
    }
}

pub(crate) fn session_needs_cloud_lookup(
    store: &Store,
    factory: &dyn ClientFactory,
    negative: &NegativeCache,
    now_ms: i64,
    coverage: &domain::FileCoverage,
    unit: UnitKey,
    media: &domain::Media,
    has_marker: bool,
) -> bool {
    if has_marker
        || media.kind != domain::MediaKind::Tv
        || coverage.units.len() != 1
        || unit.episode_coordinates().is_none()
    {
        return false;
    }
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        return false;
    };
    let library_id = coverage.file.library_id.to_string();
    let detect_intros = store
        .get_library(&library_id)
        .ok()
        .flatten()
        .is_some_and(|library| library.detect_intros);
    if !detect_intros {
        return false;
    }
    let Some(client) = configured_client(store, factory) else {
        return false;
    };
    if let Some(duration_ms) = coverage.file.duration_ms.filter(|d| *d > 0) {
        let key = LookupKey {
            file_id: coverage.file.file_id,
            version: coverage.file.version.clone(),
            unit,
            duration_ms,
            tmdb_id: tmdb_id.to_string(),
            configuration: client.cache_scope(),
        };
        if negative.contains(&key, now_ms) {
            return false;
        }
    }
    true
}

struct FlightGuard {
    registry: std::sync::Arc<InFlight>,
    target: FlightTarget,
}

impl Drop for FlightGuard {
    fn drop(&mut self) {
        self.registry.remove(&self.target);
    }
}

pub(crate) fn configured_client(
    store: &Store,
    factory: &dyn ClientFactory,
) -> Option<TheIntroDbClient> {
    let revision = store.raw_scrape_settings().ok()?.revision;
    let effective = store.get_scrape_config().ok()?.effective;
    effective.theintrodb_enabled.then(|| {
        factory
            .create(effective.theintrodb_api_key)
            .with_configuration_revision(revision)
    })
}

pub(crate) fn schedule_file_lookup(
    state: crate::management::ApiState,
    file: domain::LibraryFile,
    unit: UnitKey,
    media: domain::Media,
) {
    let target = FlightTarget {
        file_id: file.file_id,
        version: file.version.clone(),
        unit,
        media_id: media.id,
        tmdb_id: media.tmdb_id.clone().unwrap_or_default(),
    };
    let Ok(reservation) = state.chapter_work.try_reserve() else {
        tracing::debug!(file_id=%file.file_id, "chapter lookup skipped: background capacity is full");
        return;
    };
    if !state.theintrodb_inflight.insert(target.clone()) {
        return;
    }
    let guard = FlightGuard {
        registry: state.theintrodb_inflight.clone(),
        target,
    };
    tokio::spawn(async move {
        let _guard = guard;
        let factory = state.theintrodb_factory.clone();
        let current_file = file.clone();
        let current = crate::http::store_wait::store_read(
            &state,
            &[store::Db::Library, store::Db::App],
            move |store| {
                let row = store
                    .file_coverage_by_id(current_file.file_id)
                    .ok()
                    .flatten()?;
                if row.file.version != current_file.version
                    || row.file.media_id != media.id
                    || !row.units.contains(&unit)
                    || row.file.presence != domain::FilePresence::Present
                    || row.file.publication != domain::FilePublication::Published
                {
                    return None;
                }
                let current_media = store.get_media(row.file.media_id).ok().flatten()?;
                if current_media.kind != media.kind || current_media.tmdb_id != media.tmdb_id {
                    return None;
                }
                let client = configured_client(store, factory.as_ref());
                Some((row, current_media, client))
            },
        )
        .await;
        let Ok(Some((row, media, client))) = current else {
            return;
        };
        let _ = crate::marker_resolver::resolve_item_chapters_reserved(
            &state,
            &row,
            unit,
            &media,
            client,
            reservation,
        )
        .await;
    });
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct LookupKey {
    pub file_id: LedgerId,
    pub version: FileVersion,
    pub unit: UnitKey,
    pub duration_ms: i64,
    pub tmdb_id: String,
    pub configuration: u64,
}

pub(crate) struct NegativeCache(Mutex<HashMap<LookupKey, i64>>);
impl Default for NegativeCache {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}
impl NegativeCache {
    pub(crate) fn contains(&self, key: &LookupKey, now_ms: i64) -> bool {
        let mut cache = self.0.lock();
        if cache.get(key).is_some_and(|expires| *expires > now_ms) {
            return true;
        }
        cache.remove(key);
        false
    }
    pub(crate) fn insert(&self, key: LookupKey, now_ms: i64) {
        let mut cache = self.0.lock();
        cache.retain(|_, expires| *expires > now_ms);
        if cache.len() >= 4096
            && let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, expires)| *expires)
                .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
        cache.insert(key, now_ms.saturating_add(300_000));
    }
    pub(crate) fn invalidate(&self, file: LedgerId) {
        let mut cache = self.0.lock();
        cache.retain(|key, _| key.file_id != file);
    }
}
