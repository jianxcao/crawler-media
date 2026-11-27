use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use std::str::FromStr;

use domain::SiteId;
use net_guard::TargetPolicy;
use plugin_api::HOOK_WORLD;
use plugin_package::PluginManifest;
use store::Store;
use store::plugin_events::{
    ClaimDueDeliveriesInput, ClaimedOutboxDelivery, OutboxDb, OutboxEventCursor, OutboxEventRecord,
    SeedSubscriberDeliveriesInput,
};
use store::plugins::{PluginGrant, PluginLifecycle};

pub(crate) use super::event_builders::{
    build_download_after_event, build_site_checkin_after_event, build_transfer_completed_event,
    compute_relative_path, strip_library_root_rel,
};
use super::events_delivery::deliver_claimed_item;
use super::hooks_runtime::grant_covers_target_scope;
use super::ports::{HostDeps, strip_site_prefix};
use crate::management::ApiState;

pub(crate) const OUTBOX_DBS: [OutboxDb; 3] =
    [OutboxDb::App, OutboxDb::Subscribe, OutboxDb::Library];

pub(super) const EVENT_DELIVERY_LEASE_MS: i64 = 6_000;
pub(super) const EVENT_NOTIFY_TIMEOUT_MS: u64 = 5_000;
pub(super) const MAX_EVENT_ATTEMPTS: i64 = 5;
const EVENT_FANOUT_BATCH: usize = 100;
const EVENT_CLAIM_BATCH: usize = 32;
pub(super) const EVENT_RETRY_BACKOFF_MS: [i64; 4] = [250, 500, 1_000, 2_000];
const EVENT_RETENTION_MS: i64 = 7 * 86_400_000;
const EVENT_PRUNE_INTERVAL_MS: i64 = 60_000;
const EVENT_PRUNE_BATCH: usize = 200;

pub(crate) fn unix_now_ms_fallback() -> i64 {
    store::unix_now_ms().unwrap_or_else(|_| store::unix_now().saturating_mul(1_000))
}

pub(crate) fn wake(state: &ApiState) {
    state.plugins.wake_events();
}

pub(crate) fn recover_startup(state: &ApiState, now_ms: i64) {
    if !state.instance_writable() || state.write_gate.is_closed() {
        return;
    }
    let clamped_now = now_ms.max(0);
    for db in OUTBOX_DBS {
        match state
            .store
            .reclaim_orphaned_deliveries_on_startup(db, clamped_now)
        {
            Ok(count) if count > 0 => {
                tracing::info!(
                    db = db.as_str(),
                    reclaimed = count,
                    "Recovered orphaned plugin event deliveries on startup"
                );
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(
                    db = db.as_str(),
                    error = %err,
                    "Failed to recover orphaned plugin event deliveries on startup"
                );
            }
        }
    }
}

pub struct PluginEventDispatcherHandle {
    stop: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl PluginEventDispatcherHandle {
    pub async fn shutdown(self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
        self.task.await.map_err(|e| e.to_string())
    }
}

#[derive(Clone)]
pub(super) struct EventDispatchContext {
    pub(super) store: Arc<Store>,
    pub(super) plugins: Arc<super::PluginService>,
    pub(super) deps: HostDeps,
}

impl EventDispatchContext {
    fn from_state(state: &ApiState) -> Self {
        Self {
            store: state.store.clone(),
            plugins: state.plugins.clone(),
            deps: HostDeps::from_state(state),
        }
    }
}

struct WeakEventDispatchContext {
    store: Weak<Store>,
    instance_writable: Weak<AtomicBool>,
    write_gate: Weak<crate::write_gate::WriteGate>,
    plugins: Weak<super::PluginService>,
    jobs: Weak<crate::JobsWriter>,
    browser: Weak<indexer::Browser>,
    downloader: Weak<dyn downloader::Downloader>,
    self_listen: Option<SocketAddr>,
    plugin_http_policy_override: Option<TargetPolicy>,
    admin_trusted_plugins: HashSet<String>,
}

impl WeakEventDispatchContext {
    fn from_state(state: &ApiState) -> Self {
        Self {
            store: Arc::downgrade(&state.store),
            instance_writable: Arc::downgrade(&state.instance_writable),
            write_gate: Arc::downgrade(&state.write_gate),
            plugins: Arc::downgrade(&state.plugins),
            jobs: Arc::downgrade(&state.jobs),
            browser: Arc::downgrade(&state.browser),
            downloader: Arc::downgrade(&state.downloader),
            self_listen: state.self_listen,
            plugin_http_policy_override: state.plugin_http_policy_override.clone(),
            admin_trusted_plugins: state.admin_trusted_plugins.clone(),
        }
    }

    fn upgrade(
        &self,
    ) -> Option<(
        EventDispatchContext,
        Arc<AtomicBool>,
        Arc<crate::write_gate::WriteGate>,
    )> {
        let store = self.store.upgrade()?;
        let writable = self.instance_writable.upgrade()?;
        let write_gate = self.write_gate.upgrade()?;
        let plugins = self.plugins.upgrade()?;
        let jobs = self.jobs.upgrade()?;
        let browser = self.browser.upgrade()?;
        let downloader = self.downloader.upgrade()?;
        let deps = HostDeps {
            store: store.clone(),
            jobs,
            effects: plugins.effects().clone(),
            in_flight: plugins.in_flight().clone(),
            browser,
            downloader,
            instance_writable: writable.clone(),
            write_gate: write_gate.clone(),
            self_listen: self.self_listen,
            plugin_http_policy_override: self.plugin_http_policy_override.clone(),
            admin_trusted_plugins: self.admin_trusted_plugins.clone(),
        };
        Some((
            EventDispatchContext {
                store,
                plugins,
                deps,
            },
            writable,
            write_gate,
        ))
    }
}

pub(crate) fn spawn_dispatcher(state: ApiState) {
    let _ = spawn_dispatcher_with_handle(state);
}

pub(crate) fn spawn_dispatcher_with_handle(state: ApiState) -> PluginEventDispatcherHandle {
    let weak = WeakEventDispatchContext::from_state(&state);
    let notify = state.plugins.events_notify().clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_loop = stop.clone();
    let notify_loop = notify.clone();
    drop(state);

    let task = tokio::spawn(async move {
        loop {
            if stop_loop.load(Ordering::Acquire) {
                break;
            }
            let Some((ctx, writable, write_gate)) = weak.upgrade() else {
                break;
            };
            if !writable.load(Ordering::Acquire) || write_gate.is_closed() {
                drop(ctx);
                drop(writable);
                drop(write_gate);
                tokio::select! {
                    _ = notify_loop.notified() => {}
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                }
                continue;
            }
            let now_ms = unix_now_ms_fallback();
            let retry_pending = drain_ctx(&ctx, now_ms).await;
            drop(ctx);
            drop(writable);
            drop(write_gate);
            if stop_loop.load(Ordering::Acquire) {
                break;
            }
            let wait = if retry_pending {
                Duration::from_millis(250)
            } else {
                Duration::from_secs(2)
            };
            tokio::select! {
                _ = notify_loop.notified() => {}
                _ = tokio::time::sleep(wait) => {}
            }
        }
    });
    PluginEventDispatcherHandle { stop, notify, task }
}

pub(crate) async fn drain_once(state: &ApiState, now_ms: i64) -> bool {
    if !state.instance_writable() || state.write_gate.is_closed() {
        return false;
    }
    let ctx = EventDispatchContext::from_state(state);
    drain_ctx(&ctx, now_ms).await
}

async fn drain_ctx(ctx: &EventDispatchContext, now_ms: i64) -> bool {
    let fanout_store = ctx.store.clone();
    if let Err(err) =
        tokio::task::spawn_blocking(move || fanout_pending_events(&fanout_store, now_ms)).await
    {
        tracing::warn!(error = %err, "Plugin event fanout worker panicked");
    }

    let claim_store = ctx.store.clone();
    let grouped = match tokio::task::spawn_blocking(move || {
        claim_due_grouped_by_plugin(&claim_store, now_ms)
    })
    .await
    {
        Ok(Ok(grouped)) => grouped,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "Failed to claim plugin event deliveries");
            return false;
        }
        Err(err) => {
            tracing::warn!(error = %err, "Plugin event claim worker panicked");
            return false;
        }
    };

    if grouped.is_empty() {
        maybe_prune_delivered_outbox(ctx, now_ms).await;
        return false;
    }

    let mut set = tokio::task::JoinSet::new();
    for (_plugin_id, items) in grouped {
        let worker_ctx = ctx.clone();
        set.spawn_blocking(move || {
            let batch_started = std::time::Instant::now();
            let mut retry_scheduled = false;
            for (idx, (db, item)) in items.into_iter().enumerate() {
                if deliver_claimed_item(&worker_ctx, db, item, now_ms, batch_started, idx > 0) {
                    retry_scheduled = true;
                }
            }
            retry_scheduled
        });
    }

    let mut any_retry = false;
    while let Some(res) = set.join_next().await {
        match res {
            Ok(retry) => any_retry |= retry,
            Err(err) => tracing::warn!(error = %err, "Plugin event delivery worker failed"),
        }
    }
    maybe_prune_delivered_outbox(ctx, now_ms).await;
    any_retry
}

async fn maybe_prune_delivered_outbox(ctx: &EventDispatchContext, now_ms: i64) {
    let clamped_now = now_ms.max(0);
    if clamped_now < EVENT_RETENTION_MS {
        return;
    }
    if !ctx
        .plugins
        .try_claim_event_prune_slot(clamped_now, EVENT_PRUNE_INTERVAL_MS)
    {
        return;
    }
    let cutoff_ms = clamped_now - EVENT_RETENTION_MS;
    let store = ctx.store.clone();
    let _ = tokio::task::spawn_blocking(move || {
        for db in OUTBOX_DBS {
            if let Err(err) = store.prune_delivered_outbox_events(db, cutoff_ms, EVENT_PRUNE_BATCH)
            {
                tracing::warn!(
                    db = db.as_str(),
                    error = %err,
                    "Failed to prune delivered plugin outbox events"
                );
            }
        }
    })
    .await;
}

struct EligibleSubscriber {
    plugin_id: String,
    credential_bound: bool,
    manifest_has_hook: Vec<String>,
}

fn load_enabled_plugins_snapshot(store: &Store) -> Vec<EligibleSubscriber> {
    let Ok(items) = super::PluginService::list_all_plugins(store, Some(PluginLifecycle::Enabled))
    else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(items.len());
    for plugin in items {
        if plugin.lifecycle != PluginLifecycle::Enabled || !plugin.desired_enabled {
            continue;
        }
        let Ok(Some(version)) = store.get_plugin_version(&plugin.plugin_id, &plugin.package_hash)
        else {
            continue;
        };
        if version.manifest_json == "{}" {
            continue;
        }
        let Ok(manifest) = serde_json::from_str::<PluginManifest>(&version.manifest_json) else {
            continue;
        };
        for comp in manifest.components {
            if comp.world != HOOK_WORLD {
                continue;
            }
            let perms = comp.permissions;
            let hooks: Vec<String> = comp
                .hooks
                .into_iter()
                .map(|h| h.id)
                .filter(|id| {
                    let h_cap = format!("hooks:{id}");
                    let e_cap = format!("events:{id}");
                    perms.iter().any(|p| p == &h_cap) && perms.iter().any(|p| p == &e_cap)
                })
                .collect();
            if hooks.is_empty() {
                continue;
            }
            out.push(EligibleSubscriber {
                plugin_id: plugin.plugin_id.clone(),
                credential_bound: version.credential_bound,
                manifest_has_hook: hooks,
            });
        }
    }
    out
}

pub(super) fn extract_event_scope(
    event: &OutboxEventRecord,
) -> (Option<String>, Option<String>, Option<String>) {
    let non_empty = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let mut site_id = non_empty(&event.site_id);
    let mut library_id = non_empty(&event.library_id);
    let mut subscribe_id = non_empty(&event.subscribe_id);
    if (site_id.is_none() || library_id.is_none() || subscribe_id.is_none())
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&event.payload_json)
    {
        let from_json = |key: &str| {
            val.get(key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        if site_id.is_none() {
            site_id = from_json("site_id");
        }
        if library_id.is_none() {
            library_id = from_json("library_id");
        }
        if subscribe_id.is_none() {
            subscribe_id = from_json("subscribe_id");
        }
    }
    let site_id = site_id.filter(|s| {
        SiteId::from_str(strip_site_prefix(s))
            .map(|id| !id.as_uuid().is_nil())
            .unwrap_or(true)
    });
    (site_id, library_id, subscribe_id)
}

fn grants_cover_event_scope(
    grants: &[PluginGrant],
    event_kind: &str,
    site_id: Option<&str>,
    library_id: Option<&str>,
    subscribe_id: Option<&str>,
) -> bool {
    let hook_cap = format!("hooks:{event_kind}");
    let event_cap = format!("events:{event_kind}");
    let has_hook = grants.iter().any(|g| {
        g.capability == hook_cap
            && grant_covers_target_scope(&g.scope, site_id, library_id, subscribe_id)
    });
    let has_event = grants.iter().any(|g| {
        g.capability == event_cap
            && grant_covers_target_scope(&g.scope, site_id, library_id, subscribe_id)
    });
    has_hook && has_event
}

pub(super) fn plugin_holds_dual_event_grants_for_event(
    store: &Store,
    plugin_id: &str,
    event: &OutboxEventRecord,
) -> bool {
    let Ok(grants) = store.get_plugin_grants(plugin_id) else {
        return false;
    };
    let (site_id, library_id, subscribe_id) = extract_event_scope(event);
    grants_cover_event_scope(
        &grants,
        &event.event_kind,
        site_id.as_deref(),
        library_id.as_deref(),
        subscribe_id.as_deref(),
    )
}

fn fanout_pending_events(store: &Store, now_ms: i64) {
    let clamped_now = now_ms.max(0);
    let mut cached_subscribers: Option<Vec<EligibleSubscriber>> = None;
    let mut plugin_grants_cache: HashMap<String, Vec<PluginGrant>> = HashMap::new();
    let mut site_exists_cache: HashMap<String, bool> = HashMap::new();

    for db in OUTBOX_DBS {
        let mut cursor: Option<OutboxEventCursor> = None;
        loop {
            let page = match store.list_pending_fanout_events(
                db,
                cursor.as_ref(),
                Some(EVENT_FANOUT_BATCH),
            ) {
                Ok(page) => page,
                Err(err) => {
                    tracing::warn!(db = db.as_str(), error = %err, "Failed to list pending outbox fanout");
                    break;
                }
            };
            let pending = page.items;
            if pending.is_empty() {
                break;
            }
            let subs =
                cached_subscribers.get_or_insert_with(|| load_enabled_plugins_snapshot(store));
            for event in &pending {
                let (site_id, library_id, subscribe_id) = extract_event_scope(event);
                let mut plugin_ids: Vec<String> = Vec::new();
                for sub in subs.iter() {
                    if !sub.manifest_has_hook.iter().any(|h| h == &event.event_kind) {
                        continue;
                    }
                    if sub.credential_bound {
                        let Some(raw_site) = site_id.as_deref() else {
                            continue;
                        };
                        let resolvable = *site_exists_cache
                            .entry(raw_site.to_string())
                            .or_insert_with(|| {
                                SiteId::from_str(strip_site_prefix(raw_site))
                                    .ok()
                                    .and_then(|sid| store.observe_site(sid).ok().flatten())
                                    .is_some_and(|obs| obs.site().enabled)
                            });
                        if !resolvable {
                            continue;
                        }
                    }
                    let pid = &sub.plugin_id;
                    if plugin_ids.iter().any(|existing| existing == pid) {
                        continue;
                    }
                    let grants = plugin_grants_cache
                        .entry(pid.clone())
                        .or_insert_with(|| store.get_plugin_grants(pid).unwrap_or_default());
                    if grants_cover_event_scope(
                        grants,
                        &event.event_kind,
                        site_id.as_deref(),
                        library_id.as_deref(),
                        subscribe_id.as_deref(),
                    ) {
                        plugin_ids.push(pid.clone());
                    }
                }
                if let Err(err) = store.seed_event_fanout(
                    db,
                    &SeedSubscriberDeliveriesInput {
                        event_id: event.event_id.clone(),
                        plugin_ids,
                        next_attempt_at: clamped_now,
                        now_ms: clamped_now,
                    },
                ) {
                    tracing::warn!(
                        db = db.as_str(),
                        event_id = %event.event_id,
                        error = %err,
                        "Failed to seed outbox fanout deliveries"
                    );
                }
            }
            cursor = pending.last().map(|e| OutboxEventCursor {
                created_at: e.created_at,
                event_id: e.event_id.clone(),
            });
            if page.next_cursor.is_none() || pending.len() < EVENT_FANOUT_BATCH {
                break;
            }
        }
    }
}

fn claim_due_grouped_by_plugin(
    store: &Store,
    now_ms: i64,
) -> Result<BTreeMap<String, Vec<(OutboxDb, ClaimedOutboxDelivery)>>, store::StoreError> {
    let clamped_now = now_ms.max(0);
    let mut grouped: BTreeMap<String, Vec<(OutboxDb, ClaimedOutboxDelivery)>> = BTreeMap::new();
    for db in OUTBOX_DBS {
        let claimed = store.claim_due_deliveries(
            db,
            &ClaimDueDeliveriesInput {
                plugin_id: None,
                now_ms: clamped_now,
                lease_duration_ms: EVENT_DELIVERY_LEASE_MS,
                limit: Some(EVENT_CLAIM_BATCH),
            },
        )?;
        for item in claimed {
            grouped
                .entry(item.delivery.plugin_id.clone())
                .or_default()
                .push((db, item));
        }
    }
    Ok(grouped)
}
