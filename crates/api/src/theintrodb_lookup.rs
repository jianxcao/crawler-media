//! Instance-owned cloud lookup configuration and bounded negative / rate-limit state.
use crate::theintrodb::{LookupError, RateLimitScope, TheIntroDbClient};

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
use store::StoredFileMarker;

const DEFAULT_NEGATIVE_TTL_MS: i64 = 300_000;
const UNAUTHORIZED_COOLDOWN_MS: i64 = 3_600_000;
const TRANSIENT_COOLDOWN_MS: i64 = 60_000;

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
    let scope = client.cache_scope();
    if negative.is_cooling_down(scope, now_ms) {
        return false;
    }
    let key = LookupKey {
        file_id: coverage.file.file_id,
        version: coverage.file.version.clone(),
        unit,
        duration_ms: coverage.file.duration_ms.filter(|d| *d > 0),
        tmdb_id: tmdb_id.to_string(),
        configuration: scope,
    };
    !negative.contains(&key, now_ms)
}

pub(crate) struct CloudLookupRequest<'a> {
    pub(crate) client: &'a TheIntroDbClient,
    pub(crate) negative: &'a NegativeCache,
    pub(crate) file_id: LedgerId,
    pub(crate) version: &'a FileVersion,
    pub(crate) unit: UnitKey,
    pub(crate) duration_ms: Option<i64>,
    pub(crate) media_title: &'a str,
    pub(crate) tmdb_id: &'a str,
    pub(crate) now_ms: i64,
}

pub(crate) fn lookup_cloud_marker(req: CloudLookupRequest<'_>) -> Option<StoredFileMarker> {
    let (season, episode) = req.unit.episode_coordinates()?;
    let norm_duration = req.duration_ms.filter(|d| *d > 0);
    let key = LookupKey {
        file_id: req.file_id,
        version: req.version.clone(),
        unit: req.unit,
        duration_ms: norm_duration,
        tmdb_id: req.tmdb_id.to_string(),
        configuration: req.client.cache_scope(),
    };
    if req.negative.is_cooling_down(key.configuration, req.now_ms)
        || req.negative.contains(&key, req.now_ms)
    {
        return None;
    }
    let candidate =
        match req
            .client
            .get_episode_markers(req.tmdb_id, season, episode, norm_duration)
        {
            Ok(Some(candidate)) => candidate,
            Ok(None) => {
                req.negative.insert(key, req.now_ms);
                return None;
            }
            Err(error) => {
                record_lookup_error(
                    req.negative,
                    key,
                    req.media_title,
                    season,
                    episode,
                    req.now_ms,
                    error,
                );
                return None;
            }
        };
    let Some(duration) = norm_duration else {
        req.negative.insert(key, req.now_ms);
        return None;
    };
    let Some(group_duration) = candidate.group_duration_ms.filter(|d| *d > 0) else {
        req.negative.insert(key, req.now_ms);
        return None;
    };
    // v4 selects within five minutes, but silently falls back to the most
    // popular release when no cluster matches. Reject that fallback ourselves.
    if group_duration.abs_diff(duration) > 300_000 {
        tracing::warn!(
            file_id = %req.file_id,
            duration,
            group_duration,
            "TheIntroDB release duration does not match this file; candidate was not adopted"
        );
        req.negative.insert(key, req.now_ms);
        return None;
    }
    let intro = candidate.intro_start_ms.zip(candidate.intro_end_ms);
    let outro = candidate
        .outro_start_ms
        .map(|start| (start, candidate.outro_end_ms.unwrap_or(duration)));
    if intro
        .into_iter()
        .chain(outro)
        .any(|(start, end)| start < 0 || end <= start || end > duration)
        || intro
            .zip(outro)
            .is_some_and(|((_, end), (start, _))| end > start)
    {
        tracing::warn!(
            file_id = %req.file_id,
            "TheIntroDB candidate exceeds this file or has conflicting ranges"
        );
        req.negative.insert(key, req.now_ms);
        return None;
    }
    Some(StoredFileMarker {
        file_id: req.file_id,
        version: req.version.clone(),
        unit: req.unit,
        intro_start_ms: intro.map(|(s, _)| s),
        intro_end_ms: intro.map(|(_, e)| e),
        outro_start_ms: outro.map(|(s, _)| s),
        outro_end_ms: outro.map(|(_, e)| e),
        source: "theintrodb".into(),
        locked: false,
        updated_at: 0,
    })
}

fn record_lookup_error(
    negative: &NegativeCache,
    key: LookupKey,
    title: &str,
    season: u32,
    episode: u32,
    now_ms: i64,
    error: LookupError,
) {
    let scope = key.configuration;
    match &error {
        LookupError::RateLimited {
            scope: RateLimitScope::SpecificMedia,
            retry_after_ms,
        } => {
            tracing::warn!(%error, title, season, episode, "TheIntroDB 单剧集查询频次超限，暂缓重试");
            negative.insert_with_ttl(key, now_ms, *retry_after_ms);
        }
        LookupError::RateLimited {
            scope: rl_scope,
            retry_after_ms,
        } => {
            tracing::warn!(%error, ?rl_scope, retry_after_ms, title, season, episode, "TheIntroDB 触发限流/配额熔断，进入全局冷却");
            negative.trip_cooldown(scope, now_ms, *retry_after_ms);
        }
        LookupError::Unauthorized(status) => {
            tracing::warn!(
                status,
                title,
                season,
                episode,
                "TheIntroDB API Key 认证失败，冷却 1 小时"
            );
            negative.trip_cooldown(scope, now_ms, UNAUTHORIZED_COOLDOWN_MS);
        }
        LookupError::Http(status) if *status >= 500 => {
            tracing::warn!(
                status,
                title,
                season,
                episode,
                "TheIntroDB 上游异常，短冷却 60 秒"
            );
            negative.trip_cooldown(scope, now_ms, TRANSIENT_COOLDOWN_MS);
        }
        LookupError::Transport(_) => {
            tracing::warn!(%error, title, season, episode, "TheIntroDB 网络失败，短冷却 60 秒");
            negative.trip_cooldown(scope, now_ms, TRANSIENT_COOLDOWN_MS);
        }
        LookupError::EpisodeNotFound
        | LookupError::MediaNotFound
        | LookupError::Coordinates
        | LookupError::Boundaries => {
            tracing::warn!(%error, title, season, episode, "查询 TheIntroDB 无匹配结果，写入负缓存");
            negative.insert(key, now_ms);
        }
        LookupError::Http(_) | LookupError::Response(_) => {
            tracing::warn!(%error, title, season, episode, "查询 TheIntroDB 失败");
        }
    }
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
    pub duration_ms: Option<i64>,
    pub tmdb_id: String,
    pub configuration: u64,
}

#[derive(Default)]
struct NegativeState {
    entries: HashMap<LookupKey, i64>,
    cooldowns: HashMap<u64, i64>,
}

#[derive(Default)]
pub(crate) struct NegativeCache(Mutex<NegativeState>);

impl NegativeCache {
    pub(crate) fn contains(&self, key: &LookupKey, now_ms: i64) -> bool {
        let mut cache = self.0.lock();
        if cache
            .entries
            .get(key)
            .is_some_and(|expires| *expires > now_ms)
        {
            return true;
        }
        cache.entries.remove(key);
        false
    }

    pub(crate) fn insert(&self, key: LookupKey, now_ms: i64) {
        self.insert_with_ttl(key, now_ms, DEFAULT_NEGATIVE_TTL_MS);
    }

    pub(crate) fn insert_with_ttl(&self, key: LookupKey, now_ms: i64, ttl_ms: i64) {
        let mut cache = self.0.lock();
        cache.entries.retain(|_, expires| *expires > now_ms);
        if cache.entries.len() >= 4096
            && let Some(oldest) = cache
                .entries
                .iter()
                .min_by_key(|(_, expires)| *expires)
                .map(|(key, _)| key.clone())
        {
            cache.entries.remove(&oldest);
        }
        cache
            .entries
            .insert(key, now_ms.saturating_add(ttl_ms.max(1)));
    }

    pub(crate) fn is_cooling_down(&self, scope: u64, now_ms: i64) -> bool {
        let mut cache = self.0.lock();
        if cache
            .cooldowns
            .get(&scope)
            .is_some_and(|expires| *expires > now_ms)
        {
            return true;
        }
        cache.cooldowns.remove(&scope);
        false
    }

    pub(crate) fn trip_cooldown(&self, scope: u64, now_ms: i64, ttl_ms: i64) {
        let mut cache = self.0.lock();
        cache.cooldowns.retain(|_, expires| *expires > now_ms);
        if cache.cooldowns.len() >= 64
            && !cache.cooldowns.contains_key(&scope)
            && let Some(oldest) = cache
                .cooldowns
                .iter()
                .min_by_key(|(_, expires)| *expires)
                .map(|(k, _)| *k)
        {
            cache.cooldowns.remove(&oldest);
        }
        let target = now_ms.saturating_add(ttl_ms.max(1));
        cache
            .cooldowns
            .entry(scope)
            .and_modify(|expires| *expires = (*expires).max(target))
            .or_insert(target);
    }

    pub(crate) fn invalidate(&self, file: LedgerId) {
        let mut cache = self.0.lock();
        cache.entries.retain(|key, _| key.file_id != file);
    }
}
