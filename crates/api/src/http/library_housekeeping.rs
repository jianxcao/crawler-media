//! 路径核对 + 根目录归并：文件被外部移动后重新关联台账；多根指向同一物理
//! 目录时归并。两者都是「预览 → 应用」两步。
//! FS observations use the HTTP blocking budget after an owned DB snapshot;
//! reconciliation revalidates strict scope, freeze state and source before writes.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::path::Path as FsPath;

use crate::http::{err, ok};
use crate::management::ApiState;

use super::library::missing_library;
use crate::library_reconciliation::index_basenames;

const HOUSEKEEPING_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

struct Reconciliation {
    from: String,
    to: String,
    source: Option<(domain::FileCoverage, Option<domain::MediaKind>)>,
}

fn reconciliation_error(error: crate::blocking_budget::BlockingError) -> Response {
    match error {
        crate::blocking_budget::BlockingError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        crate::blocking_budget::BlockingError::Timeout => err(
            StatusCode::GATEWAY_TIMEOUT,
            "reconciliation.timeout",
            "目录索引超时",
        ),
        crate::blocking_budget::BlockingError::Worker(message) => {
            tracing::error!(%message, "Reconciliation observation worker failed");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime.worker",
                "目录核对失败",
            )
        }
    }
}

/// POST /libraries/{id}/path-reconciliation-preview — 台账失联行 + 库内新候选。
pub(crate) async fn path_reconciliation_preview(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let (library, rows) = match super::store_wait::store_read(&state, move |store| {
        let library = store
            .get_library(&id)
            .map_err(housekeeping_store_error)?
            .ok_or_else(missing_library)?;
        let rows = super::library::files_in_library(store, &library);
        Ok::<_, Response>((library, rows))
    })
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };

    let roots = library.root_paths.clone();
    let control = library::WorkControl::default();

    let preview_res = state
        .http_work
        .run(
            std::time::Duration::from_secs(30),
            move || -> Result<(Vec<serde_json::Value>, usize, bool), String> {
                let index = index_basenames(&roots, &control, 100_000)?;
                let mut missing = Vec::new();
                for row in rows {
                    let row_path = FsPath::new(&row.path);
                    if row_path.is_file() {
                        continue;
                    }
                    let name_os = row_path.file_name().unwrap_or_default();
                    let name_str = name_os.to_str().unwrap_or_default().to_string();

                    let mut candidates = Vec::new();
                    if let Some(matches) = index.candidates.get(name_os) {
                        for cand in matches {
                            if cand != row_path {
                                candidates.push(cand.display().to_string());
                            }
                        }
                    }

                    missing.push(json!({
                        "file_id": row.file_id.to_string(),
                        "media_id": row.media_id.to_string(),
                        "path": row.path.display().to_string(),
                        "file_name": name_str,
                        "candidates": candidates,
                    }));
                }
                Ok((missing, index.examined, index.incomplete))
            },
        )
        .await;

    let (missing, examined, incomplete) = match preview_res {
        Ok(Ok(data)) => data,
        Ok(Err(e)) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "reconciliation.index_failed",
                &e,
            );
        }
        Err(crate::blocking_budget::BlockingError::Busy) => {
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                "runtime.busy",
                "服务繁忙，请重试",
            );
        }
        Err(crate::blocking_budget::BlockingError::Timeout) => {
            return err(
                StatusCode::GATEWAY_TIMEOUT,
                "reconciliation.timeout",
                "目录索引超时",
            );
        }
        Err(crate::blocking_budget::BlockingError::Worker(message)) => {
            tracing::error!(%message, "目录索引 blocking worker 失败");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime.worker",
                "目录索引失败",
            );
        }
    };

    ok(json!({
        "missing": missing,
        "examined": examined,
        "incomplete": incomplete,
    }))
    .into_response()
}

/// POST /libraries/{id}/path-reconciliations — 应用重关联。
pub(crate) async fn path_reconciliations(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let capture_id = id.clone();
    let captured = super::store_wait::store_read(&state, move |store| {
        let library = store
            .get_library(&capture_id)
            .map_err(housekeeping_store_error)?
            .ok_or_else(missing_library)?;
        let ownership = store
            .library_ownership()
            .map_err(housekeeping_store_error)?;
        let rows = reconciliation_rows(store, &body).map_err(housekeeping_store_error)?;
        Ok::<_, Response>((library, ownership, rows))
    })
    .await;
    let (library, ownership, rows) = match captured {
        Ok(Ok(value)) => value,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };
    let worker = state.clone();
    let result = state
        .http_work
        .run_to_completion(move || {
            let (scope, rows) =
                observe_reconciliations(&library, ownership, rows, worker.file_stats.as_ref());
            apply_reconciliations(&worker, &library, scope.as_ref(), rows)
        })
        .await;
    match result {
        Ok((reconciled, errors)) => {
            tracing::debug!(library_id = %id, reconciled, errors = errors.len(), "Reconciliation applied");
            ok(json!({ "reconciled": reconciled, "errors": errors })).into_response()
        }
        Err(error) => {
            tracing::warn!(%error, "Reconciliation worker failed");
            reconciliation_error(error)
        }
    }
}

fn reconciliation_rows(
    store: &store::Store,
    body: &Value,
) -> Result<Vec<Reconciliation>, store::StoreError> {
    let mut rows = Vec::new();
    if let Some(input) = body["reconciliations"].as_array() {
        for row in input {
            let (Some(from), Some(to)) = (row["from"].as_str(), row["to"].as_str()) else {
                continue;
            };
            let source = match store.file_coverage_by_path(from)? {
                Some(row) => {
                    let kind = store.get_media(row.file.media_id)?.map(|media| media.kind);
                    Some((row, kind))
                }
                None => None,
            };
            rows.push(Reconciliation {
                from: from.to_owned(),
                to: to.to_owned(),
                source,
            });
        }
    }
    Ok(rows)
}

fn observe_reconciliations(
    library: &store::Library,
    ownership: store::LibraryOwnership,
    rows: Vec<Reconciliation>,
    files: &dyn crate::file_stat_read::FileStatRead,
) -> (
    Option<store::LibraryPathScope>,
    Vec<Result<Reconciliation, String>>,
) {
    let scope = match store::LibraryPathScope::resolve(ownership) {
        Ok(scope) => Some(scope),
        Err(error) => {
            tracing::warn!(%error, "Reconciliation strict scope resolution failed");
            None
        }
    };
    let observed = rows
        .into_iter()
        .map(|row| {
            if FsPath::new(&row.from).exists() {
                return Err(format!("{}: 原文件仍然存在，不能重关联", row.from));
            }
            if !files.is_file(FsPath::new(&row.to)) {
                return Err(format!("{} 不是文件", row.to));
            }
            if !strictly_owned(
                scope.as_ref(),
                FsPath::new(&row.to),
                library.kind,
                &library.id,
            ) {
                return Err(format!("{} 不属于当前媒体库范围", row.to));
            }
            let Some((source, kind)) = row.source.as_ref() else {
                return Err(format!("{} 不在台账中", row.from));
            };
            if !kind.is_some_and(|kind| {
                strictly_owned(
                    scope.as_ref(),
                    FsPath::new(&source.file.path),
                    kind,
                    &library.id,
                )
            }) {
                return Err(format!("{} 不属于当前媒体库", row.from));
            }
            Ok(row)
        })
        .collect();
    (scope, observed)
}

fn strictly_owned(
    scope: Option<&store::LibraryPathScope>,
    path: &FsPath,
    kind: domain::MediaKind,
    id: &str,
) -> bool {
    let Some(scope) = scope else { return false };
    match store::LibraryPathScope::resolve_path(path) {
        Ok(path) => {
            matches!(scope.owner(&path, kind), store::StrictOwner::Owned(owner) if owner.id == id)
        }
        Err(error) => {
            tracing::warn!(%error, "Reconciliation path resolution failed");
            false
        }
    }
}

fn apply_reconciliations(
    state: &ApiState,
    library: &store::Library,
    scope: Option<&store::LibraryPathScope>,
    rows: Vec<Result<Reconciliation, String>>,
) -> (usize, Vec<String>) {
    // Acquire the registry lease before taking Store or any publication gate.
    let lease = state.acquire_library_work(&library.id);
    let mut reconciled = 0;
    let mut errors = Vec::new();
    for row in rows {
        let result = row.and_then(|row| {
            let lease = lease.as_ref().map_err(Clone::clone)?;
            apply_reconciliation(state, library, scope, lease, &row)
        });
        match result {
            Ok(()) => reconciled += 1,
            Err(error) => errors.push(error),
        }
    }
    (reconciled, errors)
}

fn reconciliation_plan(
    state: &ApiState,
    library: &store::Library,
    scope: Option<&store::LibraryPathScope>,
    row: &Reconciliation,
) -> Result<store::library_write::RenameIntentPlan, store::StoreError> {
    let store = state.store.as_ref();
    let current = store.library_ownership()?;
    if !scope.is_some_and(|scope| {
        scope.matches(&current)
            && scope.accepts_work(&library.id)
            && current.accepts_work(&library.id)
    }) {
        return Err(store::StoreError::Conflict(
            "Reconciliation scope changed or frozen".into(),
        ));
    }
    let Some((source, kind)) = row.source.as_ref() else {
        return Err(store::StoreError::Missing(format!(
            "{} is not recorded",
            row.from
        )));
    };
    let current = store.file_coverage_by_path(&row.from)?;
    let media = store.get_media(source.file.media_id)?;
    if current.as_ref() != Some(source) || media.as_ref().map(|media| media.kind) != *kind {
        return Err(store::StoreError::Conflict(
            "Reconciliation source changed".into(),
        ));
    }
    store.plan_file_reconciliation(
        source.file.file_id,
        &source.file.version,
        FsPath::new(&row.from),
        FsPath::new(&row.to),
    )
}

fn apply_reconciliation(
    state: &ApiState,
    library: &store::Library,
    scope: Option<&store::LibraryPathScope>,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    row: &Reconciliation,
) -> Result<(), String> {
    let result = (|| {
        let plan = reconciliation_plan(state, library, scope, row)?;
        let observed = plan.observe()?;
        let now = crate::job_loop::unix_now();
        let request = lease
            .publish(|| {
                state
                    .store
                    .as_ref()
                    .begin_observed_file_rename(observed, now)
                    .map_err(|e| e.to_string())
            })
            .map_err(store::StoreError::Conflict)?;
        // The destination already owns the bytes. Receipt and completion observations
        // still run outside the Store and publication gate, as for a physical move.
        let receipt = store::library_write::execute_rename_disk(&request)
            .inspect_err(|error| {
                if let Err(record) = state.store.as_ref().record_rename_refusal(&request.rename_id, now, error) {
                    tracing::error!(%record, rename_id = %request.rename_id, "Recording reconciliation refusal failed");
                }
            })?;
        crate::rename_work::record_disk(&state.store, &receipt, now)?;
        let completion = state
            .store
            .as_ref()
            .plan_file_rename_completion(&request.rename_id)?;
        let observed = completion.observe()?;
        lease
            .publish(|| {
                state
                    .store
                    .as_ref()
                    .complete_observed_file_rename(observed, now)
                    .map_err(|e| e.to_string())
            })
            .map_err(store::StoreError::Conflict)
    })();
    result.map_err(|error: store::StoreError| {
        tracing::warn!(library_id = %library.id, from = %row.from, to = %row.to, %error, "Reconciliation durable rename failed");
        format!("{} -> {} 台账重命名失败: {error}", row.from, row.to)
    })
}

/// POST /libraries/{id}/root-consolidation-preview — 物理目录去重。
pub(crate) async fn root_consolidation_preview(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let library = match super::store_wait::store_read(&state, move |store| {
        store
            .get_library(&id)
            .map_err(housekeeping_store_error)?
            .ok_or_else(missing_library)
    })
    .await
    {
        Ok(Ok(library)) => library,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };
    let result = state
        .http_work
        .run(HOUSEKEEPING_DEADLINE, move || {
            let mut seen = std::collections::HashMap::new();
            let mut duplicates = Vec::new();
            for root in &library.root_paths {
                let Ok(real) = std::fs::canonicalize(root) else {
                    continue;
                };
                let real = real.display().to_string();
                if let Some(keep) = seen.get(&real) {
                    duplicates.push(json!({ "keep": keep, "merge": root }));
                } else {
                    seen.insert(real, root.display().to_string());
                }
            }
            duplicates
        })
        .await;
    match result {
        Ok(duplicates) => ok(json!({ "duplicates": duplicates })).into_response(),
        Err(error) => {
            tracing::warn!(%error, "Root consolidation observation failed");
            reconciliation_error(error)
        }
    }
}

fn housekeeping_store_error(error: store::StoreError) -> Response {
    tracing::error!(%error,"读取Library核对事实失败");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}
