//! Daily scheduled Discover wall prewarm job (`discover_refresh`) and
//! singleflight background Stale-While-Revalidate (SWR) helpers.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use crate::catalog::Catalog;
use crate::http::discover::{
    Section, douban_movie_sections, douban_tv_sections, movie_sections, tv_sections,
};
use crate::management::ApiState;

const REFRESH_CONCURRENCY: usize = 2;

static IN_FLIGHT_SWR: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn swr_flights() -> &'static Mutex<HashSet<String>> {
    IN_FLIGHT_SWR.get_or_init(|| Mutex::new(HashSet::new()))
}

struct SwrFlightGuard(String);

impl SwrFlightGuard {
    fn try_enter(key: String) -> Option<Self> {
        let mut set = swr_flights().lock();
        if set.contains(&key) {
            return None;
        }
        set.insert(key.clone());
        Some(Self(key))
    }
}

impl Drop for SwrFlightGuard {
    fn drop(&mut self) {
        swr_flights().lock().remove(&self.0);
    }
}

pub(crate) fn resolve_source<'a>(catalog: &'a dyn Catalog, name: &str) -> Option<&'a dyn Catalog> {
    catalog
        .source(name)
        .or_else(|| (catalog.source_name() == name).then_some(catalog))
}

pub(crate) fn select_catalog<'a>(
    catalog: &'a dyn Catalog,
    source: Option<&str>,
) -> &'a dyn Catalog {
    source
        .filter(|name| !name.is_empty())
        .and_then(|name| resolve_source(catalog, name))
        .unwrap_or(catalog)
}

pub(crate) fn run(state: &ApiState) -> Result<(), String> {
    let tmdb_configured = state
        .store
        .get_setting(crate::settings_keys::TMDB_API_KEY)
        .map_err(|error| error.to_string())?
        .is_some_and(|value| !value.trim().is_empty());

    let catalog = state.catalog.as_ref();
    let mut tasks: Vec<(&'static str, Section)> = Vec::new();
    if tmdb_configured && resolve_source(catalog, "tmdb").is_some() {
        tasks.extend(movie_sections().into_iter().map(|s| ("tmdb", s)));
        tasks.extend(tv_sections().into_iter().map(|s| ("tmdb", s)));
    }
    if resolve_source(catalog, "douban").is_some() {
        tasks.extend(douban_movie_sections().into_iter().map(|s| ("douban", s)));
        tasks.extend(douban_tv_sections().into_iter().map(|s| ("douban", s)));
    }
    if tasks.is_empty() {
        tracing::debug!("发现页榜单每日预热：暂无可用数据源，跳过");
        return Ok(());
    }

    let control = state.job_loop_control();
    let (succeeded, failed, first_error) =
        refresh_tasks_bounded(catalog, &tasks, || control.is_stopping());

    tracing::info!(tmdb_configured, succeeded, failed, "发现页榜单每日预热完成");
    if succeeded == 0 && failed > 0 && tmdb_configured {
        return Err(format!(
            "发现页榜单预热全部失败（{failed} 个分区）: {}",
            first_error.unwrap_or_default()
        ));
    }
    Ok(())
}

pub(crate) fn spawn_swr_sections_refresh(
    state: &ApiState,
    flight_key: String,
    source: Option<String>,
    sections: Vec<Section>,
) {
    if sections.is_empty() || state.job_loop_control().is_stopping() {
        return;
    }
    let Some(guard) = SwrFlightGuard::try_enter(flight_key) else {
        return;
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let catalog: Arc<dyn Catalog> = state.catalog.clone();
    let control = state.job_loop_control();
    handle.spawn_blocking(move || {
        let _guard = guard;
        let selected = select_catalog(catalog.as_ref(), source.as_deref());
        let cursor = AtomicUsize::new(0);
        let workers = sections.len().clamp(1, REFRESH_CONCURRENCY);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        if control.is_stopping() {
                            break;
                        }
                        let idx = cursor.fetch_add(1, Ordering::Relaxed);
                        let Some(section) = sections.get(idx) else {
                            break;
                        };
                        if let Err(error) = (section.fetch)(selected) {
                            tracing::warn!(
                                source = selected.source_name(),
                                kind = section.kind,
                                section = section.id,
                                %error,
                                "发现页 SWR 后台刷新单分区失败"
                            );
                        }
                    }
                });
            }
        });
    });
}

pub(crate) fn spawn_swr_closure_refresh(
    state: &ApiState,
    flight_key: String,
    work: impl FnOnce(&dyn Catalog) + Send + 'static,
) {
    if state.job_loop_control().is_stopping() {
        return;
    }
    let Some(guard) = SwrFlightGuard::try_enter(flight_key) else {
        return;
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let catalog: Arc<dyn Catalog> = state.catalog.clone();
    handle.spawn_blocking(move || {
        let _guard = guard;
        work(catalog.as_ref());
    });
}

fn refresh_tasks_bounded(
    catalog: &dyn Catalog,
    tasks: &[(&'static str, Section)],
    should_stop: impl Fn() -> bool + Sync,
) -> (usize, usize, Option<String>) {
    let cursor = AtomicUsize::new(0);
    let succeeded = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let first_error: Mutex<Option<String>> = Mutex::new(None);
    let workers = tasks.len().clamp(1, REFRESH_CONCURRENCY);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    if should_stop() {
                        break;
                    }
                    let idx = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some((source_name, section)) = tasks.get(idx) else {
                        break;
                    };
                    let Some(target) = resolve_source(catalog, source_name) else {
                        continue;
                    };
                    match (section.fetch)(target) {
                        Ok(_) => {
                            succeeded.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(error) => {
                            tracing::warn!(
                                source = *source_name,
                                kind = section.kind,
                                section = section.id,
                                %error,
                                "发现页榜单预热单分区失败"
                            );
                            failed.fetch_add(1, Ordering::Relaxed);
                            let mut slot = first_error.lock();
                            if slot.is_none() {
                                *slot = Some(error);
                            }
                        }
                    }
                }
            });
        }
    });

    (
        succeeded.load(Ordering::Relaxed),
        failed.load(Ordering::Relaxed),
        first_error.into_inner(),
    )
}
