//! Generated NFO sidecars: full-file export, edit capture, and crash recovery.
//!
//! The three entry points of this module are the only glue between a metadata
//! scrape and the Store driver:
//!
//! * [`begin_sidecar_export`] runs **before** the caller rewrites the sidecar. It
//!   resolves the export target and keeps an edit found on disk as this
//!   Library's local edit (R18, never a global override); `write_nfo` clobbers scalars in place, so an edit that is not
//!   captured here is gone for good.
//! * [`finish_sidecar_export`] runs **after** the rewrite and projects effective
//!   metadata back onto the whole file, then registers it as a metadata input.
//! * [`resume_sidecar_exports`] drives everything a previous process left
//!   unfinished; it is idempotent and never rewrites a file that already holds
//!   the intended body.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use domain::{EntityScope, Media};
use store::metadata::NfoExportRequest;

use crate::management::ApiState;

/// Process-unique suffix for export identities. A database row outlives the
/// process, so the tick alone is not enough to keep ids distinct.
static NFO_EXPORT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Sidecar a generated export owns for `video`.
pub(crate) fn sidecar_path(video: &Path) -> Option<PathBuf> {
    let stem = video.file_stem().and_then(|value| value.to_str())?;
    Some(video.with_file_name(format!("{stem}.nfo")))
}

/// Outcome of preparing a guarded sidecar export.
#[allow(clippy::large_enum_variant)]
pub(crate) enum ExportPreparation {
    /// The video is ledgered: export only through the CAS/digest driver.
    Ledgered(NfoExportRequest),
    /// The video is explicitly not in the ledger; the caller's authorized
    /// unledgered write path applies.
    NotLedgered,
}

/// Prepare the export for `video` and capture edits the sidecar holds.
///
/// Any failure (intent lookup, sidecar read, local-edit capture) is an `Err`:
/// the caller stops this file's export and never falls back to an unguarded
/// write, because that would discard an edit the export could not capture.
pub(crate) fn begin_sidecar_export(
    state: &ApiState,
    media: &Media,
    video: &Path,
    scope: EntityScope,
) -> Result<ExportPreparation, store::StoreError> {
    let Some(sidecar) = sidecar_path(video) else {
        return Ok(ExportPreparation::NotLedgered);
    };
    let (operation_id, export_id) = next_ids();
    // S01: the Store mutex is held only for database phases; the sidecar is read
    // between them and every decision is re-made against current rows.
    let target = state.store.lock().nfo_export_target_unobserved(
        video,
        &sidecar,
        scope,
        &operation_id,
        &export_id,
    );
    let mut request = match target {
        Ok(Some(request)) => request,
        Ok(None) => return Ok(ExportPreparation::NotLedgered),
        Err(error) => {
            tracing::error!(%error, media_id = %media.id, path = %video.display(), "NFO 导出意图无法建立，本文件不导出");
            return Err(error);
        }
    };
    let observation = store::metadata::export_driver::observe_nfo_sidecar(&sidecar, scope)
        .inspect_err(|error| {
            tracing::error!(%error, media_id = %media.id, path = %sidecar.display(), "NFO sidecar 无法读取，本文件不导出");
        })?;
    request.expected_digest = observation.digest.clone();
    let edits = state
        .store
        .lock()
        .nfo_local_edits_observed(&request, &observation)
        .inspect_err(|error| {
            tracing::error!(%error, media_id = %media.id, path = %request.path.display(), "NFO sidecar 本地编辑捕获失败，本文件不导出");
        })?;
    if let Some(edits) = edits {
        if edits != domain::MetadataFields::default() {
            tracing::info!(
                media_id = %media.id,
                path = %request.path.display(),
                "NFO sidecar edits kept as this Library's local edits"
            );
        }
        request.local_edits = edits;
    }
    Ok(ExportPreparation::Ledgered(request))
}

/// Project effective metadata onto the sidecar and register it as an input.
///
/// Disk reads/writes run with the Store mutex released; the final phase
/// revalidates the operation and the file/config binding before registering.
pub(crate) fn finish_sidecar_export(state: &ApiState, request: NfoExportRequest) {
    let now_ms = state.playback_clock.now_ms();
    let lock = || state.store.lock();
    match store::metadata::export_driver::export_nfo_sidecar_phased(lock, &request, now_ms) {
        Ok(outcome) => tracing::info!(
            operation_id = %request.operation_id,
            media_id = %request.media_id,
            action = ?outcome.action,
            digest = %outcome.output_digest,
            "NFO sidecar exported"
        ),
        Err(error) => tracing::error!(
            %error,
            operation_id = %request.operation_id,
            media_id = %request.media_id,
            path = %request.path.display(),
            "NFO sidecar export failed"
        ),
    }
}

/// Drive every unfinished export now; returns how many were driven.
pub(crate) fn resume_sidecar_exports(state: &ApiState, now_ms: i64) -> usize {
    let pending = match state.store.lock().pending_nfo_exports() {
        Ok(pending) => pending,
        Err(error) => {
            tracing::error!(%error, "恢复未完成的 NFO 导出失败");
            return 0;
        }
    };
    let mut driven = 0;
    for request in pending {
        let lock = || state.store.lock();
        match store::metadata::export_driver::resume_nfo_export_phased(lock, &request, now_ms) {
            Ok(outcome) => {
                driven += 1;
                tracing::info!(
                    export_id = %outcome.export_id,
                    action = ?outcome.action,
                    digest = %outcome.output_digest,
                    "中断的 NFO 导出已推进"
                );
            }
            Err(error) => tracing::error!(
                %error,
                operation_id = %request.operation_id,
                export_id = %request.export_id,
                path = %request.path.display(),
                "NFO 导出恢复失败"
            ),
        }
    }
    driven
}

#[cfg(test)]
mod tests;

fn next_ids() -> (String, String) {
    let tick = crate::job_loop::unix_now();
    let seq = NFO_EXPORT_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    (
        format!("nfo-export-{pid}-{tick}-{seq}"),
        format!("nfo-{pid}-{tick}-{seq}"),
    )
}
