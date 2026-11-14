//! Recognition state is separate from the lifecycle of its completed sampling job.
use super::{ProbeManager, ProbeUnit};
use crate::ScrapeStoreExt;

pub(super) fn prepare_json(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
    reason: &str,
) -> Result<String, store::StoreError> {
    let duration = mgr
        .store
        .get_media_info_cache_version(&unit.row.file.file_id.to_string())?
        .and_then(|metadata| metadata.format_duration_ms);
    let sample_secs = mgr
        .store
        .get_scrape_config()?
        .effective
        .fingerprint_duration_secs;
    let context_key =
        crate::fingerprint_job::key_for_file(&mgr.store, &unit.row.file, sample_secs, duration);
    Ok(serde_json::to_string(&serde_json::json!({
        "status":"uncertain", "reason":reason,
        "context_key":context_key,
        "source_version":unit.row.file.version.source_version,
        "source_revision":unit.row.file.version.revision,
        "analysis_policy_key":crate::fingerprint_job::analysis_policy_key(&marker::SamplingPolicy::default())
    }))?)
}
