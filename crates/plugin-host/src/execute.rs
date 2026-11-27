use crate::ports::State;
use crate::workers::{map_wasm_error, trap};
use crate::{CompiledIndexer, CompiledMaintenance, Invocation};
use plugin_api::{
    ApiError, Category, Descriptor, ErrorCode, Failure, HealthState, HttpRequest,
    MaintenanceAction, MaintenanceOutcome, SearchQuery, StatsView, TorrentBatch, TorrentRaw,
};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use wasmtime::Store;

const MAX_SEALED_REFS: usize = 16_384;
static NEXT_REF_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SealScope {
    pub plugin_id: String,
    pub provider_id: String,
    pub generation: u64,
    pub site_id: Option<String>,
    pub credential_epoch: Option<String>,
}

struct SealedEntry {
    scope: SealScope,
    raw_ref: String,
}

#[derive(Default)]
pub(crate) struct SealedRefStore {
    entries: HashMap<String, SealedEntry>,
    order: VecDeque<String>,
}

impl SealedRefStore {
    pub fn seal(&mut self, scope: SealScope, raw_ref: String) -> String {
        let seq = NEXT_REF_SEQ.fetch_add(1, Ordering::Relaxed);
        let token = format!("dlref-{seq}");
        if self.entries.len() >= MAX_SEALED_REFS
            && let Some(oldest) = self.order.pop_front()
        {
            self.entries.remove(&oldest);
        }
        self.order.push_back(token.clone());
        self.entries
            .insert(token.clone(), SealedEntry { scope, raw_ref });
        token
    }

    pub fn unseal(&self, token: &str, expected: &SealScope) -> Result<String, Failure> {
        let Some(entry) = self.entries.get(token) else {
            return Err(Failure::Guest(ApiError {
                code: ErrorCode::StaleHandle,
                message: "unknown or expired download_ref".into(),
                detail: None,
                retry_after_ms: None,
                operation_id: None,
            }));
        };
        if entry.scope.plugin_id != expected.plugin_id
            || entry.scope.provider_id != expected.provider_id
            || entry.scope.site_id != expected.site_id
            || entry.scope.credential_epoch != expected.credential_epoch
        {
            return Err(Failure::Guest(ApiError {
                code: ErrorCode::StaleHandle,
                message: "download_ref scope mismatch".into(),
                detail: None,
                retry_after_ms: None,
                operation_id: None,
            }));
        }
        if entry.scope.generation != expected.generation {
            return Err(Failure::Guest(ApiError {
                code: ErrorCode::StaleGeneration,
                message: "download_ref generation mismatch".into(),
                detail: None,
                retry_after_ms: None,
                operation_id: None,
            }));
        }
        Ok(entry.raw_ref.clone())
    }
}

pub(crate) fn seal_scope(inv: &Invocation, provider_id: &str) -> SealScope {
    SealScope {
        plugin_id: inv.plugin_id.clone(),
        provider_id: provider_id.to_string(),
        generation: inv.generation,
        site_id: crate::http_site_ports::canonical_site_id(inv.site_id.as_deref())
            .map(str::to_string),
        credential_epoch: inv.credential_epoch.clone(),
    }
}

pub(crate) fn validate_torrent_raw(item: &TorrentRaw) -> Result<(), Failure> {
    if item.provider_local_id.trim().is_empty() {
        return Err(Failure::InvalidInput("empty provider_local_id".into()));
    }
    if item.title.trim().is_empty() {
        return Err(Failure::InvalidInput("empty torrent title".into()));
    }
    if item.download_ref.trim().is_empty() {
        return Err(Failure::InvalidInput("empty download_ref".into()));
    }
    Ok(())
}

pub(crate) fn execute_validate_indexer(
    indexer: &CompiledIndexer,
    invocation: &Invocation,
    config_schema: &str,
    state_schema: u32,
    store: &mut Store<State>,
) -> Result<(), Failure> {
    let b = indexer.pre.instantiate(&mut *store).map_err(trap)?;
    b.crawler_plugin_lifecycle()
        .call_validate(store, config_schema, state_schema)
        .map_err(|e| map_wasm_error(invocation, e))?
        .map_err(|e| Failure::Guest(e.into()))
}

pub(crate) fn execute_validate_maintenance(
    maint: &CompiledMaintenance,
    invocation: &Invocation,
    config_schema: &str,
    state_schema: u32,
    store: &mut Store<State>,
) -> Result<(), Failure> {
    let b = maint.pre.instantiate(&mut *store).map_err(trap)?;
    b.crawler_plugin_lifecycle()
        .call_validate(store, config_schema, state_schema)
        .map_err(|e| map_wasm_error(invocation, e))?
        .map_err(|e| Failure::Guest(e.into()))
}

pub(crate) fn execute_health_indexer(
    indexer: &CompiledIndexer,
    invocation: &Invocation,
    store: &mut Store<State>,
) -> Result<HealthState, Failure> {
    let b = indexer.pre.instantiate(&mut *store).map_err(trap)?;
    b.crawler_plugin_lifecycle()
        .call_health(store)
        .map_err(|e| map_wasm_error(invocation, e))?
        .map(Into::into)
        .map_err(|e| Failure::Guest(e.into()))
}

pub(crate) fn execute_health_maintenance(
    maint: &CompiledMaintenance,
    invocation: &Invocation,
    store: &mut Store<State>,
) -> Result<HealthState, Failure> {
    let b = maint.pre.instantiate(&mut *store).map_err(trap)?;
    b.crawler_plugin_lifecycle()
        .call_health(store)
        .map_err(|e| map_wasm_error(invocation, e))?
        .map(Into::into)
        .map_err(|e| Failure::Guest(e.into()))
}

pub(crate) fn execute_indexer_describe(
    indexer: &CompiledIndexer,
    invocation: &Invocation,
    store: &mut Store<State>,
) -> Result<Vec<Descriptor>, Failure> {
    let b = indexer.pre.instantiate(&mut *store).map_err(trap)?;
    let list = b
        .crawler_plugin_indexer()
        .call_describe(store)
        .map_err(|e| map_wasm_error(invocation, e))?;
    Ok(list.into_iter().map(Into::into).collect())
}

pub(crate) fn execute_indexer_search(
    indexer: &CompiledIndexer,
    invocation: &Invocation,
    query: &SearchQuery,
    store: &mut Store<State>,
) -> Result<TorrentBatch, Failure> {
    let b = indexer.pre.instantiate(&mut *store).map_err(trap)?;
    let wit_q = query.clone().into();
    b.crawler_plugin_indexer()
        .call_search(store, &wit_q)
        .map_err(|e| map_wasm_error(invocation, e))?
        .map(Into::into)
        .map_err(|e| Failure::Guest(e.into()))
}

pub(crate) fn execute_indexer_rss(
    indexer: &CompiledIndexer,
    invocation: &Invocation,
    cursor: Option<&str>,
    store: &mut Store<State>,
) -> Result<TorrentBatch, Failure> {
    let b = indexer.pre.instantiate(&mut *store).map_err(trap)?;
    b.crawler_plugin_indexer()
        .call_rss(store, cursor)
        .map_err(|e| map_wasm_error(invocation, e))?
        .map(Into::into)
        .map_err(|e| Failure::Guest(e.into()))
}

pub(crate) fn execute_indexer_detail(
    indexer: &CompiledIndexer,
    invocation: &Invocation,
    torrent_ref: &str,
    store: &mut Store<State>,
) -> Result<TorrentRaw, Failure> {
    let b = indexer.pre.instantiate(&mut *store).map_err(trap)?;
    b.crawler_plugin_indexer()
        .call_detail(store, torrent_ref)
        .map_err(|e| map_wasm_error(invocation, e))?
        .map(Into::into)
        .map_err(|e| Failure::Guest(e.into()))
}

pub(crate) fn execute_indexer_resolve_download(
    indexer: &CompiledIndexer,
    invocation: &Invocation,
    raw_ref: &str,
    store: &mut Store<State>,
) -> Result<HttpRequest, Failure> {
    let b = indexer.pre.instantiate(&mut *store).map_err(trap)?;
    let req: HttpRequest = b
        .crawler_plugin_indexer()
        .call_resolve_download(store, raw_ref)
        .map_err(|e| map_wasm_error(invocation, e))?
        .map(Into::into)
        .map_err(|e| Failure::Guest(e.into()))?;
    if req.method.trim().is_empty() || req.max_bytes == 0 {
        return Err(Failure::InvalidInput(
            "resolve_download returned invalid HttpRequest".into(),
        ));
    }
    Ok(req)
}

pub(crate) fn execute_indexer_user_stats(
    indexer: &CompiledIndexer,
    invocation: &Invocation,
    store: &mut Store<State>,
) -> Result<StatsView, Failure> {
    let b = indexer.pre.instantiate(&mut *store).map_err(trap)?;
    b.crawler_plugin_indexer()
        .call_user_stats(store)
        .map_err(|e| map_wasm_error(invocation, e))?
        .map(Into::into)
        .map_err(|e| Failure::Guest(e.into()))
}

pub(crate) fn execute_indexer_categories(
    indexer: &CompiledIndexer,
    invocation: &Invocation,
    store: &mut Store<State>,
) -> Result<Vec<Category>, Failure> {
    let b = indexer.pre.instantiate(&mut *store).map_err(trap)?;
    let list = b
        .crawler_plugin_indexer()
        .call_categories(store)
        .map_err(|e| map_wasm_error(invocation, e))?
        .map_err(|e| Failure::Guest(e.into()))?;
    Ok(list.into_iter().map(Into::into).collect())
}

pub(crate) fn execute_maintenance_run(
    maint: &CompiledMaintenance,
    invocation: &Invocation,
    action: MaintenanceAction,
    job_id: &str,
    store: &mut Store<State>,
) -> Result<MaintenanceOutcome, Failure> {
    let b = maint.pre.instantiate(&mut *store).map_err(trap)?;
    b.crawler_plugin_maintenance()
        .call_run(store, action.into(), job_id)
        .map_err(|e| map_wasm_error(invocation, e))?
        .map(Into::into)
        .map_err(|e| Failure::Guest(e.into()))
}
