use std::collections::{HashMap, HashSet};

use domain::DownloaderId;
use downloader::{DownloadOutcome, DownloadSubmitError, FrozenDownloadPlan, SubmissionPurpose};
use sha2::{Digest, Sha256};
use store::{DownloadOperationRecord, DownloadOperationState, StoreError};

pub(super) struct SnapshotData {
    pub(super) site: Option<domain::Site>,
    pub(super) site_provider: Option<String>,
    pub(super) allowed_downloaders: Vec<String>,
    pub(super) endpoint_by_id: HashMap<DownloaderId, String>,
    pub(super) resolved_downloader_id: DownloaderId,
}

pub(super) fn map_store_error(err: StoreError) -> DownloadSubmitError {
    match err {
        StoreError::WriteBusy | StoreError::ReadBusy | StoreError::ReadDeadline => {
            DownloadSubmitError::Busy(err.to_string())
        }
        StoreError::Plugin(store::plugins::PluginError::Conflict) => {
            DownloadSubmitError::Conflict(err.to_string())
        }
        other => DownloadSubmitError::Storage(other.to_string()),
    }
}

pub(super) fn purpose_label(p: SubmissionPurpose) -> &'static str {
    match p {
        SubmissionPurpose::Subscribe => "subscribe",
        SubmissionPurpose::Manual => "manual",
        SubmissionPurpose::Admit => "admit",
        SubmissionPurpose::Boost => "boost",
        SubmissionPurpose::Plugin => "plugin",
    }
}

pub(super) fn dedup_tags(tags: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(tags.len());
    for tag in tags {
        if !tag.is_empty() && seen.insert(tag.clone()) {
            out.push(tag.clone());
        }
    }
    out
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn compute_request_digest(plan: &FrozenDownloadPlan, dl_id: DownloaderId) -> String {
    let rel = plan.release_override.as_ref().map(|r| {
        format!(
            "{}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}",
            r.title, r.year, r.season, r.episode, r.resolution, r.source, r.codec, r.hdr
        )
    });
    let summary = format!(
        "{}:{}:{}:{:?}:{}:{}:{}:{:?}:{:?}:{}:{:?}:{:?}",
        purpose_label(plan.purpose),
        plan.torrent.enclosure,
        plan.torrent.site_id,
        plan.subscribe_id,
        plan.torrent.title,
        dl_id,
        plan.paused,
        plan.save_path,
        plan.category,
        plan.force,
        plan.tags,
        rel
    );
    sha256_hex(summary.as_bytes())
}

pub(crate) fn replay_outcome(op: &DownloadOperationRecord) -> Option<DownloadOutcome> {
    let op_id = op.operation_id.parse().ok()?;
    match op.state {
        DownloadOperationState::Submitted => Some(DownloadOutcome::Submitted {
            operation_id: op_id,
            downloader_id: op.downloader_id,
            info_hash: op.info_hash.clone(),
            bytes_digest: op.bytes_digest.clone(),
            verified_total_bytes: op.verified_bytes,
            already_owned: op.outcome_code.as_deref() == Some("existing_owned"),
        }),
        DownloadOperationState::Rejected => Some(DownloadOutcome::Rejected {
            operation_id: op_id,
            reason: op
                .error_message
                .clone()
                .unwrap_or_else(|| "rejected".to_string()),
            broken_task_hash: (op.outcome_code.as_deref() == Some("existing_broken"))
                .then(|| op.info_hash.clone())
                .flatten(),
        }),
        DownloadOperationState::Unknown => Some(DownloadOutcome::Unknown {
            operation_id: op_id,
            downloader_id: op.downloader_id,
            reason: op
                .error_message
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        }),
        DownloadOperationState::Intent | DownloadOperationState::Reserved => {
            Some(DownloadOutcome::Unknown {
                operation_id: op_id,
                downloader_id: op.downloader_id,
                reason: "operation is currently in progress".to_string(),
            })
        }
        DownloadOperationState::Submitting => Some(DownloadOutcome::Unknown {
            operation_id: op_id,
            downloader_id: op.downloader_id,
            reason: "operation is currently submitting".to_string(),
        }),
        _ => None,
    }
}

pub(super) fn resolve_active_subscribe_defer(
    store: &store::Store,
    subscribe_id: domain::SubscribeId,
    enclosure: &str,
    now_utc: i64,
) {
    let Ok(defers) = store.list_subscribe_defers(subscribe_id, 100) else {
        return;
    };
    for defer in defers {
        if defer.enclosure == enclosure
            && defer.state == store::SubscribeDeferState::Deferred
            && let Err(err) =
                store.resolve_subscribe_defer(&defer.defer_id, defer.revision, now_utc)
        {
            tracing::warn!(
                %err,
                subscribe_id = %subscribe_id,
                defer_id = %defer.defer_id,
                "failed to resolve active subscribe defer"
            );
        }
    }
}

pub(super) fn validate_initial_plan_inputs(
    plan: &FrozenDownloadPlan,
) -> Result<(), DownloadSubmitError> {
    let allowed_sub_tag = plan.subscribe_id.map(|sub_id| format!("cm-sub:{sub_id}"));
    for tag in &plan.tags {
        if tag.trim() != tag
            || tag.is_empty()
            || tag.contains(',')
            || tag.chars().any(char::is_control)
        {
            return Err(DownloadSubmitError::InvalidPlan(format!(
                "invalid download tag `{tag}`"
            )));
        }
        if allowed_sub_tag.as_deref() != Some(tag.as_str())
            && downloader::is_reserved_system_tag(tag)
        {
            return Err(DownloadSubmitError::InvalidPlan(format!(
                "reserved system download tag `{tag}` is not permitted"
            )));
        }
    }
    if let Some(cat) = &plan.category
        && let Err(reason) = downloader::validate_category_name(cat)
    {
        return Err(DownloadSubmitError::InvalidPlan(reason.into()));
    }
    if matches!(
        plan.purpose,
        SubmissionPurpose::Plugin | SubmissionPurpose::Boost
    ) && let Some(raw_dir) = plan.save_path.as_deref().filter(|s| !s.trim().is_empty())
    {
        validate_plugin_relative_dir(raw_dir)?;
    }
    Ok(())
}

fn validate_plugin_relative_dir(raw: &str) -> Result<(), DownloadSubmitError> {
    use std::path::{Component, Path};

    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.contains('\0')
        || trimmed.chars().any(char::is_control)
        || trimmed.contains('\\')
        || trimmed.starts_with('/')
        || trimmed.ends_with('/')
    {
        return Err(DownloadSubmitError::InvalidPlan(
            "invalid relative_directory".into(),
        ));
    }
    let mut has_seg = false;
    for comp in Path::new(trimmed).components() {
        match comp {
            Component::Normal(seg) if seg.to_str().is_some() => has_seg = true,
            _ => {
                return Err(DownloadSubmitError::InvalidPlan(
                    "unsafe path component in relative_directory".into(),
                ));
            }
        }
    }
    if !has_seg {
        return Err(DownloadSubmitError::InvalidPlan(
            "empty relative_directory".into(),
        ));
    }
    Ok(())
}
