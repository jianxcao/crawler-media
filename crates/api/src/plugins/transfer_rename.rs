use std::collections::HashMap;
use std::path::{Path, PathBuf};

use domain::{LibraryId, Media, MediaId, MediaKind, Subscribe, SubscribeId};
use downloader::Downloader;
use filter::ScoredTorrent;
use hooks::HookTrace;
use plugin_api::TransferRename;
use subscribe::collection_destinations::DestinationMapping;

use super::hooks_catalog_and_traces::record_hook_trace_audit;
use super::hooks_runtime::{base_hook_context, build_hook_pipeline};
use crate::management::ApiState;

#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct TransferRenameRequest {
    pub workflow_id: String,
    pub library_id: LibraryId,
    pub library_root: PathBuf,
    pub media_id: MediaId,
    pub media_kind: MediaKind,
    pub subscribe_id: Option<SubscribeId>,
    pub source_path: PathBuf,
    pub proposed_relative_path: PathBuf,
}

#[doc(hidden)]
pub fn run_transfer_rename(state: &ApiState, req: &TransferRenameRequest) -> PathBuf {
    let default_dest = req.library_root.join(&req.proposed_relative_path);
    let pipeline = build_hook_pipeline(state, "transfer.rename");

    let original_rel = req.proposed_relative_path.to_string_lossy().into_owned();
    let mut input = TransferRename {
        media_id: req.media_id.to_string(),
        library_id: req.library_id.to_string(),
        source_path: req.source_path.to_string_lossy().into_owned(),
        proposed_relative_path: original_rel.clone(),
        is_movie: req.media_kind == MediaKind::Movie,
    };

    let mut ctx = base_hook_context(None, req.subscribe_id.map(|s| s.to_string()), 500);
    ctx.library_id = Some(req.library_id.to_string());

    let trace = match pipeline.filter_transfer_rename_with_trace(&req.workflow_id, &ctx, &mut input)
    {
        Ok(trace) => trace,
        Err(reason) => {
            tracing::warn!(workflow_id = %req.workflow_id, error = %reason, "transfer.rename pipeline aborted; falling back to default destination");
            return default_dest;
        }
    };
    if trace.steps.is_empty() {
        return default_dest;
    }

    let (final_dest, boundary_rejected) =
        validate_renamed_destination(state, req, &original_rel, &input.proposed_relative_path);
    record_rename_audit(state, &req.workflow_id, &trace, boundary_rejected);
    if !trace.steps.is_empty()
        && let Some(sub_id) = req.subscribe_id
    {
        state.plugins.record_subscribe_trace(sub_id, trace);
    }
    final_dest
}

pub(crate) fn has_transfer_rename_subscribers(state: &ApiState) -> bool {
    !build_hook_pipeline(state, "transfer.rename").is_empty()
}

pub(crate) fn plan_subscribe_renamed_destinations<D: Downloader + ?Sized>(
    state: &ApiState,
    subscribe: &Subscribe,
    media: &Media,
    library_id: LibraryId,
    library_root: &Path,
    naming: Option<&str>,
    transfer_mode: Option<library::TransferMode>,
    absolute: Option<&subscribe::AbsoluteEpisodes>,
    downloader: &D,
    chosen: &[ScoredTorrent],
    existing: &[DestinationMapping],
) -> HashMap<PathBuf, PathBuf> {
    let mut renamed = HashMap::new();
    if chosen.is_empty() || !has_transfer_rename_subscribers(state) {
        return renamed;
    }
    for scored in chosen {
        let planned = subscribe::plan_torrent_destinations(
            subscribe,
            media,
            library_root,
            naming,
            transfer_mode,
            absolute,
            downloader,
            scored,
            existing,
        );
        for (src_path, proposed_dest) in planned {
            let Ok(rel) = proposed_dest.strip_prefix(library_root) else {
                continue;
            };
            let dest = run_transfer_rename(
                state,
                &TransferRenameRequest {
                    workflow_id: format!("subscribe:{}", subscribe.id),
                    library_id,
                    library_root: library_root.to_path_buf(),
                    media_id: media.id,
                    media_kind: media.kind,
                    subscribe_id: Some(subscribe.id),
                    source_path: src_path.clone(),
                    proposed_relative_path: rel.to_path_buf(),
                },
            );
            renamed.insert(src_path, dest);
        }
    }
    renamed
}

fn validate_renamed_destination(
    state: &ApiState,
    req: &TransferRenameRequest,
    original_rel: &str,
    proposed_rel: &str,
) -> (PathBuf, bool) {
    let default_dest = req.library_root.join(&req.proposed_relative_path);
    if proposed_rel == original_rel {
        return (default_dest, false);
    }
    let Ok(clean_rel) = hooks::validate_rename_path(proposed_rel) else {
        return (default_dest, true);
    };
    let candidate = req.library_root.join(&clean_rel);
    if !store::path_under_root(&candidate, &req.library_root) {
        return (default_dest, true);
    }
    let expected_root_id = state
        .store
        .library_root_owning_key(
            &req.library_root.join("__cm_root_probe__.mkv"),
            req.media_kind,
        )
        .ok()
        .flatten()
        .map(|(_, root_id)| root_id);
    match state
        .store
        .library_root_owning_key(&candidate, req.media_kind)
    {
        Ok(Some((owning_lib, owning_root))) => {
            let lib_ok = owning_lib == req.library_id.to_string();
            let root_ok = expected_root_id
                .as_ref()
                .is_none_or(|expected| expected == &owning_root);
            if lib_ok && root_ok {
                (candidate, false)
            } else {
                tracing::warn!(
                    library_id = %req.library_id,
                    candidate = %candidate.display(),
                    owning_lib,
                    owning_root,
                    "transfer.rename proposed path crosses into nested/other library root; falling back"
                );
                (default_dest, true)
            }
        }
        Ok(None) if expected_root_id.is_none() => (candidate, false),
        Ok(None) | Err(_) => (default_dest, true),
    }
}

fn record_rename_audit(
    state: &ApiState,
    workflow_id: &str,
    trace: &HookTrace,
    boundary_rejected: bool,
) {
    record_hook_trace_audit(
        state,
        "transfer.rename",
        workflow_id,
        trace,
        boundary_rejected,
    );
}
