//! Library-scoped work leases. Registry -> Store is the only permitted lock order.
use crate::management::ApiState;
mod removal;

impl ApiState {
    pub(crate) fn acquire_path_work(
        &self,
        path: &std::path::Path,
        kind: domain::MediaKind,
    ) -> Result<Option<LibraryWorkLease>, String> {
        let resolved = store::LibraryPathScope::resolve_path(path).map_err(|e| e.to_string())?;
        let owner = {
            let store = self.store.lock();
            let owner = store
                .library_owning_key(&resolved, kind)
                .map_err(|e| e.to_string())?;
            if owner.is_none()
                && store
                    .path_key_in_removal_scope(&resolved)
                    .map_err(|e| e.to_string())?
            {
                return Err("Library 已删除，拒绝旧目录工作".into());
            }
            owner
        };
        owner
            .map(|owner| self.acquire_library_work(&owner))
            .transpose()
    }
    pub fn acquire_library_work(&self, library_id: &str) -> Result<LibraryWorkLease, String> {
        self.library_work.acquire(&self.store, library_id)
    }

    /// 等该库当前在跑的后台工作全部结束（建库自动扫描、显式扫描、重导入共用同一
    /// registry；由 lease drop 的 `notify_waiters` 唤醒，不是定时轮询）。
    ///
    /// 调用方负责加超时。存在的理由：建库会自动调度首次扫描，与显式 `POST /scan`
    /// 并发，先返回的那次可能还没做完身份重绑，观察者需要一个确定性同步点。
    pub async fn wait_library_idle(&self, library_id: &str) {
        self.library_work.wait_idle(library_id).await;
    }
}

pub fn start_library_removal(
    state: &ApiState,
    library_id: &str,
) -> Result<store::LibraryRemoval, String> {
    let handle = tokio::runtime::Handle::try_current().map_err(|e| e.to_string())?;
    let operation = state
        .library_work
        .freeze(&state.store, library_id)
        .map_err(|e| e.to_string())?;
    launch_library_removal(state, operation, handle)
}

pub(crate) fn launch_library_removal(
    state: &ApiState,
    mut operation: store::LibraryRemoval,
    handle: tokio::runtime::Handle,
) -> Result<store::LibraryRemoval, String> {
    if operation.state == "succeeded" {
        return Ok(operation);
    }
    if !state
        .library_work
        .removals
        .lock()
        .insert(operation.operation_id.clone())
    {
        return Ok(operation);
    }
    if operation.state == "failed" {
        if let Err(error) =
            state
                .store
                .lock()
                .set_removal_state(&operation.operation_id, "draining", None)
        {
            state
                .library_work
                .removals
                .lock()
                .remove(&operation.operation_id);
            return Err(error.to_string());
        }
        operation.state = "draining".into();
        operation.error = None;
    }
    let state = state.clone();
    let operation_id = operation.operation_id.clone();
    handle.spawn(async move {
        if let Err(error) = drain_with_retry(&state, &operation_id).await {
            tracing::error!(%operation_id, %error, "Library 删除多次重试后仍失败，保持冻结");
            let failed_id = operation_id.clone();
            match crate::http::store_wait::store_blocking(state.store(), move |store| {
                store.set_removal_state(&failed_id, "failed", Some(&error))
            })
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(persist_error)) => {
                    tracing::error!(%operation_id, %persist_error, "保存 Library 删除失败状态失败")
                }
                Err(persist_error) => {
                    tracing::error!(%operation_id, %persist_error, "保存 Library 删除失败状态失败")
                }
            }
        }
        state.library_work.removals.lock().remove(&operation_id);
        crate::root_reassign_work::notify(&state);
    });
    Ok(operation)
}

/// Backoff between automatic drain attempts. A transient failure (busy disk,
/// slow mount, a racing writer) must not leave a Library hidden but uncleaned
/// until someone clicks delete again.
const DRAIN_RETRY_BACKOFF: [std::time::Duration; 3] = [
    std::time::Duration::from_secs(5),
    std::time::Duration::from_secs(30),
    std::time::Duration::from_secs(120),
];

async fn drain_with_retry(state: &ApiState, operation_id: &str) -> Result<(), String> {
    let mut attempt = 0;
    loop {
        let error = match removal::drain(state, operation_id).await {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        let Some(delay) = DRAIN_RETRY_BACKOFF.get(attempt).copied() else {
            return Err(error);
        };
        attempt += 1;
        tracing::warn!(%operation_id, %error, attempt, ?delay, "Library 删除失败，稍后自动重试");
        let owned = operation_id.to_owned();
        let note = format!("retry {attempt} after error: {error}");
        let _ = crate::http::store_wait::store_blocking(state.store(), move |store| {
            store.set_removal_state(&owned, "draining", Some(&note))
        })
        .await;
        tokio::time::sleep(delay).await;
    }
}

pub fn resume_library_removals(state: &ApiState) -> Result<(), String> {
    let operations = state
        .store
        .lock()
        .pending_library_removals()
        .map_err(|e| e.to_string())?;
    for operation in operations {
        state.library_work.cancel(&operation.library_id);
        if operation.state == "failed" {
            tracing::warn!(operation_id = %operation.operation_id, library_id = %operation.library_id,
                "Library 删除此前已失败，启动时自动重试");
        }
        start_library_removal(state, &operation.library_id)?;
    }
    Ok(())
}
use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;

#[derive(Default)]
pub struct LibraryWorkRegistry {
    entries: Mutex<HashMap<String, Entry>>,
    changed: Notify,
    removals: Mutex<std::collections::HashSet<String>>,
    /// Operation ids whose driver task is already running in this process. A
    /// reconfiguration is driven once per process; startup recovery and a new 202
    /// can both ask for the same operation.
    drivers: Mutex<std::collections::HashSet<String>>,
    pub(crate) root_changed: Arc<Notify>,
    pub(crate) root_driver: std::sync::atomic::AtomicBool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LibraryWorkKind {
    Ordinary,
    AutomaticDetection,
    ExplicitDetection,
}

#[derive(Default)]
struct Entry {
    cancelled: bool,
    /// Set while a persistent reconfiguration owns this Library. Unlike
    /// `cancelled` it is scoped to one operation and cleared when that operation
    /// ends, so a failed reconfiguration does not block the Library forever.
    blocked_by: Option<String>,
    controls: HashMap<String, (LibraryWorkKind, library::WorkControl)>,
}

pub struct LibraryWorkLease {
    registry: Arc<LibraryWorkRegistry>,
    library_id: String,
    lease_id: String,
    control: library::WorkControl,
}

impl LibraryWorkLease {
    pub fn library_id(&self) -> &str {
        &self.library_id
    }
    pub fn publish<T>(&self, publish: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        // Lock order is registry → (released) → publication gate. The registry
        // guard is dropped before entering the gate: a freeze that marks the
        // entry after this check still cancels `self.control` under the gate, so
        // the gate's own cancelled check refuses or the freeze waits for us.
        let cancelled = self
            .registry
            .entries
            .lock()
            .get(&self.library_id)
            .is_none_or(|entry| entry.cancelled);
        if cancelled {
            return Err("Library work cancelled".into());
        }
        self.control.publish(publish)
    }
    pub fn control(&self) -> &library::WorkControl {
        &self.control
    }
}

impl Drop for LibraryWorkLease {
    fn drop(&mut self) {
        if let Some(entry) = self.registry.entries.lock().get_mut(&self.library_id) {
            entry.controls.remove(&self.lease_id);
        }
        self.registry.changed.notify_waiters();
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum BoundedLeaseError {
    #[error("Library Registry busy")]
    RegistryBusy,
    #[error("Library Store busy")]
    StoreBusy,
    #[error("Library 正在重建索引，暂时拒绝新的工作")]
    Reconfiguring,
    #[error("Library 正在删除或已不存在")]
    Unavailable,
    #[error("Library optional detection disabled")]
    OptionalDisabled,
    #[error("{0}")]
    Store(String),
}

impl LibraryWorkRegistry {
    pub fn acquire(
        self: &Arc<Self>,
        store: &Mutex<store::Store>,
        library_id: &str,
    ) -> Result<LibraryWorkLease, String> {
        let mut entries = self.entries.lock();
        let entry = entries.entry(library_id.to_string()).or_default();
        if entry.blocked_by.is_some() {
            return Err("Library 正在重建索引，暂时拒绝新的工作".into());
        }
        if entry.cancelled
            || !store
                .lock()
                .library_accepts_work(library_id)
                .map_err(|e| e.to_string())?
        {
            return Err("Library 正在删除或已不存在".into());
        }
        let lease_id = uuid::Uuid::new_v4().to_string();
        let control = library::WorkControl::default();
        entry.controls.insert(
            lease_id.clone(),
            (LibraryWorkKind::Ordinary, control.clone()),
        );
        Ok(LibraryWorkLease {
            registry: self.clone(),
            library_id: library_id.into(),
            lease_id,
            control,
        })
    }
    pub(crate) fn acquire_bounded(
        self: &Arc<Self>,
        store: &Mutex<store::Store>,
        library_id: &str,
    ) -> Result<LibraryWorkLease, BoundedLeaseError> {
        self.acquire_bounded_kind(store, library_id, LibraryWorkKind::Ordinary)
    }

    pub(crate) fn acquire_from_readers(
        self: &Arc<Self>,
        readers: &Arc<crate::store_readers::StoreReaders>,
        library_id: &str,
    ) -> Result<LibraryWorkLease, BoundedLeaseError> {
        self.acquire_bounded_with(library_id, LibraryWorkKind::Ordinary, || {
            readers
                .checkout(crate::http::store_wait::STORE_READ_WAIT)
                .ok_or(BoundedLeaseError::StoreBusy)
        })
    }

    pub(crate) fn acquire_optional_from_readers(
        self: &Arc<Self>,
        readers: &Arc<crate::store_readers::StoreReaders>,
        library_id: &str,
        kind: LibraryWorkKind,
    ) -> Result<LibraryWorkLease, BoundedLeaseError> {
        self.acquire_bounded_with(library_id, kind, || {
            readers
                .checkout(crate::http::store_wait::STORE_READ_WAIT)
                .ok_or(BoundedLeaseError::StoreBusy)
        })
    }

    fn acquire_bounded_kind(
        self: &Arc<Self>,
        store: &Mutex<store::Store>,
        library_id: &str,
        kind: LibraryWorkKind,
    ) -> Result<LibraryWorkLease, BoundedLeaseError> {
        self.acquire_bounded_with(library_id, kind, || {
            store
                .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
                .ok_or(BoundedLeaseError::StoreBusy)
        })
    }

    fn acquire_bounded_with<R: std::ops::Deref<Target = store::Store>>(
        self: &Arc<Self>,
        library_id: &str,
        kind: LibraryWorkKind,
        acquire: impl FnOnce() -> Result<R, BoundedLeaseError>,
    ) -> Result<LibraryWorkLease, BoundedLeaseError> {
        let mut entries = self
            .entries
            .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
            .ok_or(BoundedLeaseError::RegistryBusy)?;
        let entry = entries.entry(library_id.to_string()).or_default();
        if entry.blocked_by.is_some() {
            return Err(BoundedLeaseError::Reconfiguring);
        }
        let store = acquire()?;
        if entry.cancelled
            || !store
                .library_accepts_work(library_id)
                .map_err(|e| BoundedLeaseError::Store(e.to_string()))?
        {
            return Err(BoundedLeaseError::Unavailable);
        }
        if kind == LibraryWorkKind::AutomaticDetection {
            let enabled = store
                .get_library(library_id)
                .map_err(|e| BoundedLeaseError::Store(e.to_string()))?
                .is_some_and(|library| library.detect_intros && library.enable_fingerprint);
            if !enabled {
                return Err(BoundedLeaseError::OptionalDisabled);
            }
        }
        let lease_id = uuid::Uuid::new_v4().to_string();
        let control = library::WorkControl::default();
        entry
            .controls
            .insert(lease_id.clone(), (kind, control.clone()));
        Ok(LibraryWorkLease {
            registry: self.clone(),
            library_id: library_id.into(),
            lease_id,
            control,
        })
    }

    /// Registry -> Store serializes switch publication with optional admission.
    /// WorkControl gates are cancelled only after both guards have been dropped.
    pub(crate) fn set_optional_settings(
        &self,
        state: &ApiState,
        library_id: &str,
        detect: Option<bool>,
        fingerprint: Option<bool>,
    ) -> Result<Result<(), store::StoreError>, axum::response::Response> {
        let controls = {
            let mut entries = self
                .entries
                .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
                .ok_or_else(crate::http::store_wait::busy_response)?;
            let store = state
                .store
                .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
                .ok_or_else(crate::http::store_wait::busy_response)?;
            state.require_writable()?;
            state.require_write_admitted()?;
            let current = match store.get_library(library_id) {
                Ok(Some(current)) => current,
                Ok(None) => return Ok(Err(store::StoreError::Missing(library_id.into()))),
                Err(error) => return Ok(Err(error)),
            };
            let detect = detect.unwrap_or(current.detect_intros);
            let fingerprint = fingerprint.unwrap_or(current.enable_fingerprint);
            let disabling =
                (current.detect_intros && !detect) || (current.enable_fingerprint && !fingerprint);
            if let Err(error) = store.set_library_intro_settings(library_id, detect, fingerprint) {
                return Ok(Err(error));
            }
            if disabling {
                entries
                    .entry(library_id.into())
                    .or_default()
                    .controls
                    .values()
                    .filter(|(kind, _)| *kind != LibraryWorkKind::Ordinary)
                    .map(|(_, control)| control.clone())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        };
        for control in controls {
            control.cancel();
        }
        Ok(Ok(()))
    }
    pub fn freeze(
        &self,
        store: &Mutex<store::Store>,
        library_id: &str,
    ) -> Result<store::LibraryRemoval, store::StoreError> {
        let controls = {
            let mut entries = self.entries.lock();
            let operation = store.lock().begin_library_removal(library_id)?;
            let entry = entries.entry(library_id.into()).or_default();
            entry.cancelled = true;
            let controls: Vec<_> = entry
                .controls
                .values()
                .map(|(_, control)| control.clone())
                .collect();
            (operation, controls)
        };
        // Cancel outside the registry: `cancel` waits on each publication gate,
        // and a publication may acquire or drop a lease (registry) inside it.
        // No new lease can be added any more because `cancelled` is already set.
        let (operation, controls) = controls;
        for control in controls {
            control.cancel();
        }
        Ok(operation)
    }

    /// Lease held by a reconfiguration driver while it applies one batch.
    ///
    /// The driver is the only writer allowed on the Library, so it takes a lease
    /// through the same registry as worker writes: a Library removal that starts
    /// mid-operation still wins, because `cancelled` refuses this lease too.
    pub(crate) fn acquire_for_reconfiguration(
        self: &Arc<Self>,
        store: &Mutex<store::Store>,
        library_id: &str,
        operation_id: &str,
    ) -> Result<LibraryWorkLease, String> {
        let mut entries = self
            .entries
            .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
            .ok_or_else(|| "Registry busy while acquiring reconfiguration work".to_string())?;
        let entry = entries.entry(library_id.to_string()).or_default();
        if entry.cancelled {
            return Err("Library work cancelled".into());
        }
        if entry.blocked_by.as_deref() != Some(operation_id) {
            return Err(format!("Library 未被重配置操作 {operation_id} 冻结"));
        }
        if !store
            .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
            .ok_or_else(|| "Store busy while acquiring reconfiguration work".to_string())?
            .library_accepts_work(library_id)
            .map_err(|e| e.to_string())?
        {
            return Err("Library 正在删除或已不存在".into());
        }
        let lease_id = uuid::Uuid::new_v4().to_string();
        let control = library::WorkControl::default();
        entry.controls.insert(
            lease_id.clone(),
            (LibraryWorkKind::Ordinary, control.clone()),
        );
        Ok(LibraryWorkLease {
            registry: self.clone(),
            library_id: library_id.into(),
            lease_id,
            control,
        })
    }

    /// Mark a Library as owned by one persistent reconfiguration until `release`.
    ///
    /// Infallible and Store-free on purpose: `begin_library_reconfiguration` already
    /// committed the freeze, and this only mirrors it in the registry so that
    /// in-process workers stop before touching a frozen generation.
    pub(crate) fn block(&self, library_id: &str, operation_id: &str) {
        let controls: Vec<_> = {
            let mut entries = self.entries.lock();
            let entry = entries.entry(library_id.into()).or_default();
            entry.blocked_by = Some(operation_id.into());
            entry
                .controls
                .values()
                .map(|(_, control)| control.clone())
                .collect()
        };
        // Same order as `freeze`: never wait on a publication gate under the registry.
        for control in controls {
            control.cancel();
        }
    }

    /// Clear the block, but only for the operation that owns it.
    pub(crate) fn release(&self, library_id: &str, operation_id: &str) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.get_mut(library_id)
            && entry.blocked_by.as_deref() == Some(operation_id)
        {
            entry.blocked_by = None;
        }
    }

    /// Claim the single in-process driver slot for one operation. `false` means a
    /// driver for the same operation is already running here.
    pub(crate) fn claim_driver(&self, operation_id: &str) -> bool {
        self.drivers.lock().insert(operation_id.to_string())
    }

    pub(crate) fn release_driver(&self, operation_id: &str) {
        self.drivers.lock().remove(operation_id);
    }

    pub(crate) fn active_control(
        &self,
        library_id: &str,
        kind: LibraryWorkKind,
    ) -> Option<library::WorkControl> {
        self.entries.lock().get(library_id).and_then(|entry| {
            entry
                .controls
                .values()
                .find(|(active_kind, control)| *active_kind == kind && !control.is_cancelled())
                .map(|(_, control)| control.clone())
        })
    }

    /// 是否有任何 Library 工作正在进行（维护任务据此跳过重型操作）。
    ///
    /// 这只是**降低冲突**：返回 false 不代表之后不会被新扫描抢到锁，SQLite
    /// busy 处理与有界执行仍然必须存在。
    pub fn has_active_work(&self) -> bool {
        self.entries
            .lock()
            .values()
            .any(|entry| !entry.controls.is_empty())
    }

    pub fn cancel(&self, library_id: &str) {
        let controls: Vec<library::WorkControl> = {
            let mut entries = self.entries.lock();
            let entry = entries.entry(library_id.into()).or_default();
            entry.cancelled = true;
            entry
                .controls
                .values()
                .map(|(_, control)| control.clone())
                .collect()
        };
        // Same order as `freeze`/`block`: a control may wait on its publication gate,
        // and that publication may take the registry to acquire/drop another lease.
        for control in controls {
            control.cancel();
        }
    }

    pub async fn wait_idle(&self, library_id: &str) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            // A Registry writer may be waiting for Store in Registry -> Store
            // order. Do not queue an executor thread behind that writer.
            let idle = self.entries.try_lock().map(|entries| {
                entries
                    .get(library_id)
                    .is_none_or(|entry| entry.controls.is_empty())
            });
            match idle {
                Some(true) => return,
                Some(false) => notified.await,
                None => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            }
        }
    }
}
