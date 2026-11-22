//! Daily scheduled Discover wall prewarm job (`discover_refresh`) and
//! singleflight background Stale-While-Revalidate (SWR) helpers.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use tokio::sync::Semaphore;

use crate::catalog::Catalog;
use crate::http::discover::{
    Section, douban_movie_sections, douban_tv_sections, movie_sections, tv_sections,
};
use crate::management::ApiState;

const REFRESH_CONCURRENCY: usize = 2;
const MAX_IN_FLIGHT_SWR: usize = 16;

struct SwrInstanceFlights {
    keys: HashSet<String>,
    semaphore: Arc<Semaphore>,
}

static IN_FLIGHT_SWR: OnceLock<Mutex<HashMap<usize, SwrInstanceFlights>>> = OnceLock::new();

fn swr_flights() -> &'static Mutex<HashMap<usize, SwrInstanceFlights>> {
    IN_FLIGHT_SWR.get_or_init(|| Mutex::new(HashMap::new()))
}

struct SwrFlightGuard {
    instance_id: usize,
    flight_key: String,
    semaphore: Arc<Semaphore>,
    // Keep the per-instance allocation alive until after `instance_id` leaves `IN_FLIGHT_SWR`.
    _anchor: Arc<crate::write_gate::WriteGate>,
}

impl SwrFlightGuard {
    fn try_enter(state: &ApiState, flight_key: String) -> Option<Self> {
        let anchor = state.write_gate();
        let instance_id = Arc::as_ptr(&anchor) as usize;
        let mut map = swr_flights().lock();
        let entry = map
            .entry(instance_id)
            .or_insert_with(|| SwrInstanceFlights {
                keys: HashSet::new(),
                semaphore: Arc::new(Semaphore::new(REFRESH_CONCURRENCY)),
            });
        if entry.keys.contains(&flight_key) || entry.keys.len() >= MAX_IN_FLIGHT_SWR {
            if entry.keys.is_empty() {
                map.remove(&instance_id);
            }
            return None;
        }
        entry.keys.insert(flight_key.clone());
        let semaphore = entry.semaphore.clone();
        Some(Self {
            instance_id,
            flight_key,
            semaphore,
            _anchor: anchor,
        })
    }
}

impl Drop for SwrFlightGuard {
    fn drop(&mut self) {
        let mut map = swr_flights().lock();
        if let Some(entry) = map.get_mut(&self.instance_id) {
            entry.keys.remove(&self.flight_key);
            if entry.keys.is_empty() {
                map.remove(&self.instance_id);
            }
        }
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
    if !state.instance_writable() || state.write_gate().admit().is_err() {
        return;
    }
    if sections.is_empty() || state.job_loop_control().is_stopping() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(guard) = SwrFlightGuard::try_enter(state, flight_key) else {
        return;
    };
    let catalog: Arc<dyn Catalog> = state.catalog.clone();
    let control = state.job_loop_control();
    let writable = state.instance_writable.clone();
    let write_gate = state.write_gate();
    let semaphore = guard.semaphore.clone();
    handle.spawn(async move {
        let _guard = guard;
        let Ok(_permit) = semaphore.acquire_owned().await else {
            return;
        };
        if control.is_stopping() || !writable.load(Ordering::SeqCst) || write_gate.admit().is_err()
        {
            return;
        }
        let _ = tokio::task::spawn_blocking(move || {
            let selected = select_catalog(catalog.as_ref(), source.as_deref());
            let cursor = AtomicUsize::new(0);
            let workers = sections.len().clamp(1, REFRESH_CONCURRENCY);
            std::thread::scope(|scope| {
                for _ in 0..workers {
                    scope.spawn(|| {
                        loop {
                            if control.is_stopping()
                                || !writable.load(Ordering::SeqCst)
                                || write_gate.admit().is_err()
                            {
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
        })
        .await;
    });
}

pub(crate) fn spawn_swr_closure_refresh(
    state: &ApiState,
    flight_key: String,
    work: impl FnOnce(&dyn Catalog) + Send + 'static,
) {
    if !state.instance_writable() || state.write_gate().admit().is_err() {
        return;
    }
    if state.job_loop_control().is_stopping() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(guard) = SwrFlightGuard::try_enter(state, flight_key) else {
        return;
    };
    let catalog: Arc<dyn Catalog> = state.catalog.clone();
    let control = state.job_loop_control();
    let writable = state.instance_writable.clone();
    let write_gate = state.write_gate();
    let semaphore = guard.semaphore.clone();
    handle.spawn(async move {
        let _guard = guard;
        let Ok(_permit) = semaphore.acquire_owned().await else {
            return;
        };
        if control.is_stopping() || !writable.load(Ordering::SeqCst) || write_gate.admit().is_err()
        {
            return;
        }
        let _ = tokio::task::spawn_blocking(move || {
            work(catalog.as_ref());
        })
        .await;
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
