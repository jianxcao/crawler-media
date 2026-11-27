use std::path::{Path, PathBuf};

use domain::{LibraryId, MediaId, SiteId, SubscribeId};
use plugin_api::{DownloadAfterEvent, SiteCheckinAfterEvent, TransferCompletedEvent};
use store::plugin_events::EmitOutboxEventInput;
use store::{DownloadOperationRecord, DownloadOperationState, Store};
use uuid::Uuid;

pub(crate) fn compute_relative_path(
    store: &Store,
    library_id: LibraryId,
    dest_path: &Path,
) -> String {
    let roots: Vec<PathBuf> = store
        .get_library(&library_id.to_string())
        .ok()
        .flatten()
        .map(|lib| lib.root_paths)
        .unwrap_or_default();
    strip_library_root_rel(&roots, dest_path)
}

pub(crate) fn strip_library_root_rel(roots: &[PathBuf], dest_path: &Path) -> String {
    let mut best: Option<String> = None;
    let canonical_dest = std::fs::canonicalize(dest_path).ok();
    for root in roots {
        let rel = dest_path.strip_prefix(root).ok().or_else(|| {
            let canon_root = std::fs::canonicalize(root).ok()?;
            canonical_dest.as_deref()?.strip_prefix(&canon_root).ok()
        });
        if let Some(rel) = rel {
            let candidate = rel.to_string_lossy().into_owned();
            if best.as_ref().is_none_or(|b| candidate.len() < b.len()) {
                best = Some(candidate);
            }
        }
    }
    best.unwrap_or_else(|| {
        dest_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dest_path.to_string_lossy().into_owned())
    })
}

pub(crate) fn build_site_checkin_after_event(
    site_id: SiteId,
    window_key: &str,
    outcome_state: &str,
    now_ms: i64,
) -> Option<EmitOutboxEventInput> {
    let site_str = site_id.to_string();
    let payload = SiteCheckinAfterEvent {
        site_id: site_str.clone(),
        action: "checkin".to_string(),
        state: outcome_state.to_string(),
        window_key: window_key.to_string(),
    };
    let payload_json = serde_json::to_string(&payload).ok()?;
    let handles_json = serde_json::to_string(&[&site_str]).ok()?;
    Some(EmitOutboxEventInput {
        event_id: format!(
            "evt-checkin-{site_str}-{window_key}-{}",
            Uuid::new_v4().simple()
        ),
        schema_version: 1,
        event_domain: "site".to_string(),
        event_kind: "site.checkin.after".to_string(),
        entity_type: "site".to_string(),
        entity_id: site_str.clone(),
        entity_sequence: None,
        entity_revision: 1,
        cause_id: format!("checkin:{site_str}:{window_key}"),
        subject: "system".to_string(),
        site_id: Some(site_str),
        subscribe_id: None,
        library_id: None,
        handles_json,
        payload_json,
        occurred_at: now_ms.max(0),
        created_at: now_ms.max(0),
    })
}

pub(crate) fn build_download_after_event(
    op: &DownloadOperationRecord,
    resolved_state: DownloadOperationState,
    info_hash: Option<&str>,
    verified_total_bytes: Option<i64>,
    now_ms: i64,
) -> Option<EmitOutboxEventInput> {
    let outcome = match resolved_state {
        DownloadOperationState::Submitted => "submitted",
        DownloadOperationState::Rejected => "rejected",
        DownloadOperationState::Unknown => "unknown",
        DownloadOperationState::Failed => "failed",
        _ => return None,
    };
    let payload = DownloadAfterEvent {
        operation_id: op.operation_id.clone(),
        downloader_id: op.downloader_id.map(|id| id.to_string()),
        info_hash: info_hash
            .map(str::to_string)
            .or_else(|| op.info_hash.clone()),
        outcome: outcome.to_string(),
        verified_total_bytes: verified_total_bytes.and_then(|v| u64::try_from(v).ok()),
    };
    let payload_json = serde_json::to_string(&payload).ok()?;
    let handles_json = serde_json::to_string(&[&op.operation_id]).ok()?;
    Some(EmitOutboxEventInput {
        event_id: format!("evt-dl-{}-{}", op.operation_id, Uuid::new_v4().simple()),
        schema_version: 1,
        event_domain: "download".to_string(),
        event_kind: "download.after".to_string(),
        entity_type: "download_operation".to_string(),
        entity_id: op.operation_id.clone(),
        entity_sequence: None,
        entity_revision: op.revision.saturating_add(1).max(1),
        cause_id: format!("download:{}", op.operation_id),
        subject: "system".to_string(),
        site_id: op
            .site_id
            .filter(|id| !id.as_uuid().is_nil())
            .map(|id| id.to_string()),
        subscribe_id: op.subscribe_id.map(|id| id.to_string()),
        library_id: None,
        handles_json,
        payload_json,
        occurred_at: now_ms.max(0),
        created_at: now_ms.max(0),
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_transfer_completed_event(
    media_id: MediaId,
    library_id: LibraryId,
    subscribe_id: Option<SubscribeId>,
    relative_path: String,
    file_count: u64,
    total_bytes: u64,
    cause_id: String,
    now_ms: i64,
) -> Option<EmitOutboxEventInput> {
    let media_str = media_id.to_string();
    let lib_str = library_id.to_string();
    let payload = TransferCompletedEvent {
        media_id: media_str.clone(),
        library_id: lib_str.clone(),
        relative_path,
        file_count: u32::try_from(file_count).unwrap_or(u32::MAX),
        total_bytes,
    };
    let payload_json = serde_json::to_string(&payload).ok()?;
    let handles_json = serde_json::to_string(&[&media_str]).ok()?;
    let cause = if cause_id.trim().is_empty() {
        format!("transfer:{media_str}")
    } else {
        cause_id
    };
    Some(EmitOutboxEventInput {
        event_id: format!("evt-transfer-{media_str}-{}", Uuid::new_v4().simple()),
        schema_version: 1,
        event_domain: "transfer".to_string(),
        event_kind: "transfer.completed".to_string(),
        entity_type: "media".to_string(),
        entity_id: media_str,
        entity_sequence: None,
        entity_revision: 1,
        cause_id: cause,
        subject: "system".to_string(),
        site_id: None,
        subscribe_id: subscribe_id.map(|id| id.to_string()),
        library_id: Some(lib_str),
        handles_json,
        payload_json,
        occurred_at: now_ms.max(0),
        created_at: now_ms.max(0),
    })
}
