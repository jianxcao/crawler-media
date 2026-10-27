//! Persistent cross-Library operations: start, observe, resume, and answer "is this
//! Library frozen right now?".
//!
//! A root or locale reconfiguration rewrites `library_files` and NFO ownership in
//! batches, so it cannot run inside one HTTP request: `PATCH /libraries/{id}` returns
//! `202` plus an operation id, and this module owns the driver that applies batches
//! until the operation is terminal. The batch state machine itself stays in the Store
//! (`begin_library_reconfiguration` / `advance_library_operation`); nothing here
//! invents a second copy of it.
//!
//! Lock order stays Registry -> Store: registry blocks are taken without a Store guard
//! held, and `block` / `release` never touch the Store.

use std::str::FromStr;
use std::time::Duration;

use serde_json::Value;

use crate::management::ApiState;
use crate::store::{LibraryReconfiguration, StoreError, library_operations::LibraryOperationView};

/// Media rows migrated per applied batch; the Store rejects anything outside 1..=100.
const BATCH_MEDIA: usize = 100;
/// How long in-flight Library work may take to stop before the driver stands aside.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// The Library fields a caller wants migrated. At least one must be set.
#[derive(Debug, Clone, Default)]
pub struct ReconfigurationRequest {
    /// Replacement root list, first entry = primary root.
    pub root_paths: Option<Vec<String>>,
    /// New display locale. Existing NFO observations keep their own language.
    pub display_locale: Option<String>,
}

/// Why a reconfiguration was not started.
#[derive(Debug)]
pub enum StorageOperationError {
    /// No such Library.
    MissingLibrary,
    /// A persistent operation already owns this Library. `operation_id` is absent when
    /// only the Store barrier explains the freeze (the row outlived its operation).
    Reindexing {
        operation_id: Option<String>,
    },
    /// The request carries nothing usable.
    Invalid(String),
    Store(StoreError),
}

impl std::fmt::Display for StorageOperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingLibrary => f.write_str("library not found"),
            Self::Reindexing { operation_id } => match operation_id {
                Some(operation_id) => write!(f, "library is reindexing ({operation_id})"),
                None => f.write_str("library is reindexing"),
            },
            Self::Invalid(message) => f.write_str(message),
            Self::Store(error) => write!(f, "{error}"),
        }
    }
}

/// Start a reconfiguration and describe it exactly like `GET .../operations/{id}` does.
///
/// The `202` body and the poll answer share [`operation_json`], so a client can POST and
/// immediately poll the same shape. At least one field must be set; an empty `root_paths`
/// list counts as "not set" because dropping every root is never a reconfiguration.
pub fn begin_library_reconfiguration(
    state: &ApiState,
    library_id: &str,
    request: ReconfigurationRequest,
) -> Result<Value, StorageOperationError> {
    let root_paths = request.root_paths.filter(|roots| !roots.is_empty());
    if root_paths.is_none() && request.display_locale.is_none() {
        return Err(StorageOperationError::Invalid(
            "root_paths or display_locale is required".into(),
        ));
    }
    let (expected_revision, operation_id) = {
        let store = state.store.lock();
        if store
            .get_library(library_id)
            .map_err(StorageOperationError::Store)?
            .is_none()
        {
            return Err(StorageOperationError::MissingLibrary);
        }
        let lifecycle = store
            .library_lifecycle_state(library_id)
            .map_err(StorageOperationError::Store)?;
        if lifecycle != "ready" {
            let operation_id = store
                .active_library_operation(library_id)
                .map_err(StorageOperationError::Store)?;
            return Err(StorageOperationError::Reindexing { operation_id });
        }
        let revision = store
            .library_config_revision(library_id)
            .map_err(StorageOperationError::Store)?;
        (revision, uuid::Uuid::new_v4().to_string())
    };
    let now_ms = crate::job_loop::unix_now();
    // 上面的 `get_library` 已经确认该 id 存在，因此解析失败只可能来自畸形输入，
    // 一律按“不存在”回答，绝不 panic。
    let library_ref = domain::LibraryId::from_str(library_id)
        .map_err(|_| StorageOperationError::MissingLibrary)?;
    let prepared = LibraryReconfiguration {
        operation_id: operation_id.clone(),
        library_id: library_ref,
        expected_revision,
        root_paths: root_paths
            .clone()
            .map(|roots| roots.into_iter().map(std::path::PathBuf::from).collect()),
        display_locale: request.display_locale.clone(),
    }
    .prepare()
    .map_err(StorageOperationError::Store)?;
    let started = state
        .store
        .lock()
        .begin_library_reconfiguration_prepared(&prepared, now_ms);
    let status = match started {
        Ok(status) => status,
        // Another request began between the check above and the CAS (or the revision moved
        // under us): report the operation that won instead of pretending we started one.
        Err(StoreError::Protected(_)) => {
            let operation_id = state
                .store
                .lock()
                .active_library_operation(library_id)
                .unwrap_or_else(|error| {
                    tracing::warn!(%library_id, %error, "重配置冲突后读取活动操作失败");
                    None
                });
            return Err(StorageOperationError::Reindexing { operation_id });
        }
        Err(error) => return Err(StorageOperationError::Store(error)),
    };
    // `block` only after the Store committed the freeze: the registry mirror must never
    // refuse writes for an operation that does not exist.
    state.library_work.block(library_id, &operation_id);
    spawn_driver(state, &operation_id, library_id);
    let kind = if root_paths.is_some() {
        "roots"
    } else {
        "locale"
    };
    tracing::info!(
        %operation_id,
        %library_id,
        kind = kind,
        state = %status.state,
        "Library 重配置已受理"
    );
    Ok(operation_json(&LibraryOperationView {
        operation_id,
        library_id: library_id.to_string(),
        kind: kind.into(),
        phase: status.phase,
        state: status.state,
        cursor: None,
        error: status.error,
        updated_at_ms: now_ms,
    }))
}

/// The JSON shape shared by the `202` body and `GET .../operations/{operation_id}`.
pub fn operation_json(view: &LibraryOperationView) -> Value {
    serde_json::json!({
        "operation_id": view.operation_id,
        "library_id": view.library_id,
        "kind": view.kind,
        "phase": view.phase,
        "state": view.state,
        "cursor": view.cursor,
        "error": view.error,
    })
}

/// One operation for HTTP, scoped to its Library. `None` means "answer 404".
pub fn library_operation(
    state: &ApiState,
    library_id: &str,
    operation_id: &str,
) -> Result<Option<Value>, String> {
    let view = state
        .store
        .lock()
        .library_operation_view(operation_id)
        .map_err(|error| error.to_string())?;
    Ok(view
        .filter(|view| view.library_id == library_id)
        .map(|view| operation_json(&view)))
}

/// Whether a write request must be refused because the Library is being reconfigured.
#[derive(Debug, PartialEq)]
pub enum WriteVerdict {
    /// Nothing persistent owns the Library; let the request through. Any remaining
    /// refusal is the Store write barrier's job.
    Proceed,
    /// `library.reindexing`. The operation id is absent when only the frozen lifecycle
    /// row explains the refusal.
    Reindexing { operation_id: Option<String> },
}

/// The freeze verdict for a write to `library_id`.
///
/// A frozen Library is the trigger, not "an unfinished row exists": a row whose process
/// died must not block a healthy Library, while a frozen Library must block even when its
/// row is gone. Store errors answer `Proceed` so the caller keeps its own 404/500 path (an
/// unknown Library must still be 404, never 409); the Store write barrier remains the last
/// line of defence either way.
pub fn write_verdict(state: &ApiState, library_id: &str) -> WriteVerdict {
    let store = state.store.lock();
    match store.library_lifecycle_state(library_id) {
        Ok(lifecycle) if lifecycle == "reindexing" => {}
        Ok(_) => return WriteVerdict::Proceed,
        Err(error) => {
            tracing::debug!(%library_id, %error, "生命周期读取失败，交由写屏障判定");
            return WriteVerdict::Proceed;
        }
    }
    match store.active_library_operation(library_id) {
        Ok(operation_id) => WriteVerdict::Reindexing { operation_id },
        Err(error) => {
            tracing::warn!(%library_id, %error, "重配置操作读取失败，仅凭冻结状态拒绝写入");
            WriteVerdict::Reindexing { operation_id: None }
        }
    }
}

/// Resume every reconfiguration a previous process left unfinished.
///
/// Mirrors `resume_library_removals`: the caller holds the data-directory lease, so no
/// other process drives these, and background workers accept new Library work only after
/// this returns. Terminal operations are never touched, so a restart cannot re-apply an
/// already applied rewrite.
pub fn resume_library_reconfigurations(state: &ApiState) -> Result<(), String> {
    let unfinished = state
        .store
        .lock()
        .unfinished_library_operations()
        .map_err(|error| error.to_string())?;
    for operation_id in unfinished {
        let Some(view) = current_view(state, &operation_id)? else {
            tracing::warn!(%operation_id, "未完成的重配置操作已消失，跳过恢复");
            continue;
        };
        tracing::info!(
            %operation_id,
            library_id = %view.library_id,
            kind = %view.kind,
            state = %view.state,
            "恢复未完成的 Library 重配置操作"
        );
        if view.kind == store::metadata::NFO_EXPORT_KIND {
            // 单文件导出自带摘要断点，既不需要冻结整个 Library，也不需要等它闲下来：
            // 恢复直接在调用线程上把每个未完成的导出推到终态（重入安全）。
            crate::scrape_metadata::export::resume_sidecar_exports(
                state,
                crate::job_loop::unix_now(),
            );
            continue;
        }
        state.library_work.block(&view.library_id, &operation_id);
        spawn_driver(state, &operation_id, &view.library_id);
    }
    Ok(())
}

/// 推进所有未完成的 Media 合并操作。
///
/// 合并的每个阶段都写在 `media_operations` 行里，任何进程都能安全重入，因此上轮
/// 没跑完的操作（含上个进程留下的）由这里逐轮推到终态。调用方必须处于阻塞上下文
/// 并持有数据目录租约；单条失败只告警并跳过——冻结的两个身份仍是可恢复的，一条
/// 坏行不该停掉整轮后台工作。
///
/// 本轮只要有操作成功推进，就把已退休 Media 的 job payload 引用收敛到存活方：
/// 合并把 loser 的台账并进了 survivor，但队列里排着的 catalog 刷新等任务仍指着
/// loser，不收敛就会在下一轮去刷一个已被退休的 Media。
pub fn advance_media_merges(state: &ApiState, now_ms: i64) {
    let unfinished = match state.store.lock().unfinished_media_merges() {
        Ok(unfinished) => unfinished,
        Err(error) => {
            tracing::warn!(%error, "读取未完成的 Media 合并操作失败，跳过本轮");
            return;
        }
    };
    let mut advanced = false;
    for operation_id in unfinished {
        match state
            .store
            .lock()
            .advance_media_merge(&operation_id, now_ms)
        {
            Ok(status) => {
                advanced = true;
                tracing::info!(
                    %operation_id,
                    phase = %status.phase,
                    state = %status.state,
                    "Media 合并操作已推进"
                );
            }
            Err(error) => {
                tracing::warn!(%operation_id, %error, "Media 合并操作推进失败，跳过本轮")
            }
        }
    }
    if advanced {
        converge_retired_media_refs(state);
    }
}

/// 把指向已退休 Media 的 job payload 引用改写成它的存活方，返回实际改写的行数。
///
/// 幂等：引用已经收敛时返回 0，也不会留下任何持久标记；单对失败只告警，继续处理
/// 其余各对。锁序固定为「先 store（读完即释放）再 jobs」，绝不在持有 store 锁时
/// 去取 jobs 锁——jobs 与 store 是两把独立的锁，同时持有会和其他路径的取锁顺序打架。
pub fn converge_retired_media_refs(state: &ApiState) -> usize {
    let pairs = {
        let store = state.store.lock();
        store.retired_media_survivors()
    };
    let pairs = match pairs {
        Ok(pairs) => pairs,
        Err(error) => {
            tracing::warn!(%error, "读取已退休 Media 的存活方失败，跳过引用收敛");
            return 0;
        }
    };
    let jobs = state.jobs.lock();
    let mut rewritten = 0;
    for (loser, survivor) in pairs {
        match jobs.rewrite_media_refs(&loser.to_string(), &survivor.to_string()) {
            Ok(0) => {}
            Ok(n) => {
                rewritten += n;
                tracing::info!(
                    loser = %loser,
                    survivor = %survivor,
                    rewritten = n,
                    "media merge rewrote job payload references"
                );
            }
            Err(err) => tracing::warn!(
                loser = %loser,
                %err,
                "media merge job reference convergence failed"
            ),
        }
    }
    rewritten
}

pub fn begin_library_integrity(
    state: &ApiState,
    library_id: &str,
    repair: bool,
) -> Result<Value, StorageOperationError> {
    let operation_id = uuid::Uuid::new_v4().to_string();
    let library_ref = domain::LibraryId::from_str(library_id)
        .map_err(|_| StorageOperationError::MissingLibrary)?;
    let now_ms = crate::job_loop::unix_now();

    let started =
        state
            .store
            .lock()
            .begin_library_integrity(library_ref, repair, &operation_id, now_ms);
    let status = match started {
        Ok(status) => status,
        Err(StoreError::Missing(_)) => return Err(StorageOperationError::MissingLibrary),
        Err(StoreError::Protected(_)) => {
            let active = state
                .store
                .lock()
                .active_library_operation(library_id)
                .unwrap_or(None);
            return Err(StorageOperationError::Reindexing {
                operation_id: active,
            });
        }
        Err(e) => return Err(StorageOperationError::Store(e)),
    };

    state.library_work.block(library_id, &operation_id);
    spawn_driver(state, &operation_id, library_id);
    let kind = if repair {
        "integrity-repairs"
    } else {
        "integrity-checks"
    };
    Ok(operation_json(&LibraryOperationView {
        operation_id,
        library_id: library_id.to_string(),
        kind: kind.into(),
        phase: status.phase,
        state: status.state,
        cursor: None,
        error: status.error,
        updated_at_ms: now_ms,
    }))
}

/// Start the in-process driver unless one is already running for this operation.
pub(crate) fn spawn_driver(state: &ApiState, operation_id: &str, library_id: &str) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(%operation_id, %library_id, "无 tokio 运行时承载驱动，等待下次启动恢复");
        return;
    };
    if !state.library_work.claim_driver(operation_id) {
        return;
    }
    let state = state.clone();
    let operation_id = operation_id.to_string();
    let library_id = library_id.to_string();
    handle.spawn(async move {
        if let Err(error) = drive(&state, &operation_id, &library_id).await {
            // The Store keeps the row resumable; startup recovery (or the next 202) retries.
            tracing::error!(%operation_id, %library_id, %error, "Library 重配置驱动中断");
        }
        state.library_work.release(&library_id, &operation_id);
        state.library_work.release_driver(&operation_id);
        crate::root_reassign_work::notify(&state);
    });
}

/// Freeze, drain, then apply batches until the operation is terminal.
async fn drive(state: &ApiState, operation_id: &str, library_id: &str) -> Result<(), String> {
    state.library_work.block(library_id, operation_id);
    if tokio::time::timeout(DRAIN_TIMEOUT, state.library_work.wait_idle(library_id))
        .await
        .is_err()
    {
        return Err(format!(
            "Library 工作在 {} 秒内未停止，等待下次恢复",
            DRAIN_TIMEOUT.as_secs()
        ));
    }
    let Some(mut current) = current_view(state, operation_id)? else {
        tracing::warn!(%operation_id, "重配置操作不存在，驱动退出");
        return Ok(());
    };
    if terminal(&current.state)
        && !(current.state == "failed"
            && matches!(
                current.kind.as_str(),
                "roots" | "locale" | "integrity" | "default_locale"
            ))
    {
        tracing::info!(%operation_id, %library_id, state = %current.state, "重配置操作已是终态");
        return Ok(());
    }
    tracing::info!(
        %operation_id,
        %library_id,
        kind = %current.kind,
        "开始应用 Library 重配置批次"
    );
    let mut batch: u32 = 0;
    loop {
        batch += 1;
        let lease = state.library_work.acquire_for_reconfiguration(
            &state.store,
            library_id,
            operation_id,
        )?;
        let status = state
            .store
            .lock()
            .advance_library_operation(operation_id, crate::job_loop::unix_now(), BATCH_MEDIA)
            .map_err(|error| error.to_string())?;
        drop(lease);
        if terminal(&status.state) {
            tracing::info!(
                %operation_id,
                %library_id,
                batches = batch,
                state = %status.state,
                "Library 重配置结束"
            );
            return Ok(());
        }
        // The cursor is the only proof a batch moved something; without it the driver
        // would spin forever on a stuck operation.
        let cursor = current_view(state, operation_id)?.and_then(|view| view.cursor);
        if cursor == current.cursor {
            return Err(format!("第 {batch} 批未推进游标，停止驱动等待下次恢复"));
        }
        current.cursor = cursor;
        tracing::debug!(%operation_id, %library_id, batches = batch, cursor = ?current.cursor, "Library 重配置批次已应用");
        tokio::task::yield_now().await;
    }
}

fn current_view(
    state: &ApiState,
    operation_id: &str,
) -> Result<Option<LibraryOperationView>, String> {
    state
        .store
        .lock()
        .library_operation_view(operation_id)
        .map_err(|error| error.to_string())
}

fn terminal(state: &str) -> bool {
    matches!(state, "succeeded" | "failed")
}
