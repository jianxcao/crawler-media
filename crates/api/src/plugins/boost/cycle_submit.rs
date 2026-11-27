use std::collections::HashSet;

use domain::{DownloaderId, PluginOperationId, Torrent};
use downloader::{
    DownloadOpContext, DownloadOutcome, DownloadSubmitError, FrozenDownloadPlan, SubmissionPurpose,
    TorrentSource,
};
use store::download_operations::{BoostTaskFactRecord, BoostTaskState};
use store::plugins::PluginRecord;
use uuid::Uuid;

use super::super::config::BuiltinBoostConfig;
use super::{
    AuthorizedBoostSite, BoostCycleSummary, is_live_plugin_active_at, resolve_target_downloader,
};
use crate::download_service::ApiDownloadService;
use crate::download_service::plugin_command_cleanup::auto_cleanup_retained_task;
use crate::management::ApiState;

pub(super) fn discover_candidates(state: &ApiState, sites: &[AuthorizedBoostSite]) -> Vec<Torrent> {
    if sites.is_empty() {
        return Vec::new();
    }
    let snapshot = state.indexer.snapshot();
    let mut hits = Vec::new();
    for entry in sites {
        let site_slice = std::slice::from_ref(&entry.site);
        if entry.can_search {
            let outcome = state.indexer.search_page_categories_with_snapshot(
                &snapshot,
                site_slice,
                "",
                1,
                &[],
            );
            if !outcome.torrents.is_empty() {
                hits.extend(outcome.torrents);
                continue;
            }
        }
        if entry.can_rss {
            let rss = state.indexer.rss_with_snapshot(&snapshot, site_slice);
            hits.extend(rss.torrents);
        }
    }
    hits
}

pub(super) fn run_auto_cleanup_pass(
    state: &ApiState,
    plugin: &PluginRecord,
    cfg: &BuiltinBoostConfig,
    dl_id: DownloaderId,
    tasks: &[BoostTaskFactRecord],
    now: i64,
) -> usize {
    let mut cleaned = 0usize;
    for tf in tasks {
        if !is_live_plugin_active_at(state, plugin)
            || resolve_target_downloader(state, cfg) != Ok(dl_id)
        {
            break;
        }
        if tf.downloader_id != dl_id {
            continue;
        }
        if !cfg.sites.is_empty() && !tf.site_id.is_some_and(|s| cfg.sites.contains(&s)) {
            continue;
        }
        let eligible_state = tf.state == BoostTaskState::RetainedOnly
            || (tf.state == BoostTaskState::CleanupPlanned
                && tf.cleanup_plan_expires_at.is_some_and(|e| e < now));
        if !eligible_state {
            continue;
        }
        match auto_cleanup_retained_task(state, plugin, tf, now) {
            Ok(true) => cleaned += 1,
            Ok(false) => {}
            Err(err) => {
                tracing::warn!(
                    plugin_id = %plugin.plugin_id,
                    task_fact_id = %tf.task_fact_id,
                    error = %err,
                    "builtin.boost auto_cleanup_files task skipped"
                );
            }
        }
    }
    cleaned
}

pub(super) fn collect_tracked_hashes(snap: &plugin_api::BoostSnapshotView) -> HashSet<String> {
    let mut hashes = HashSet::new();
    for t in &snap.tasks {
        if t.state != "cleaned" {
            let h = t.info_hash.trim().to_ascii_lowercase();
            if !h.is_empty() {
                hashes.insert(h);
            }
        }
    }
    for r in &snap.reservations {
        if let Some(h) = r.info_hash.as_deref() {
            let norm = h.trim().to_ascii_lowercase();
            if !norm.is_empty() {
                hashes.insert(norm);
            }
        }
    }
    hashes
}

#[allow(clippy::too_many_arguments)]
pub(super) fn submit_boost_candidates(
    state: &ApiState,
    plugin: &PluginRecord,
    cfg: &BuiltinBoostConfig,
    dl_id: DownloaderId,
    quota_key: &str,
    cause_id: &str,
    candidates: Vec<Torrent>,
    mut seen_enclosures: HashSet<String>,
    tracked_hashes: &HashSet<String>,
    round_cap: usize,
    summary: &mut BoostCycleSummary,
) {
    let submitter = ApiDownloadService::new(state.clone());
    let rt = tokio::runtime::Handle::try_current();
    for torrent in candidates {
        if summary.added >= round_cap {
            break;
        }
        if !is_live_plugin_active_at(state, plugin)
            || resolve_target_downloader(state, cfg) != Ok(dl_id)
        {
            break;
        }
        if !state
            .store
            .observe_site(torrent.site_id)
            .ok()
            .flatten()
            .is_some_and(|obs| obs.site().enabled)
        {
            continue;
        }
        if cfg.free_only && !torrent.free {
            continue;
        }
        if cfg.exclude_hr && torrent.hr == Some(true) {
            continue;
        }
        if let Some(seeders) = torrent.seeders
            && i32::try_from(seeders).unwrap_or(0) < cfg.min_seeders
        {
            continue;
        }
        let enclosure = torrent.enclosure.trim();
        if enclosure.is_empty() || !seen_enclosures.insert(enclosure.to_string()) {
            continue;
        }
        if let Some(mag_hash) = downloader::magnet_info_hash(enclosure)
            && tracked_hashes.contains(&mag_hash.to_ascii_lowercase())
        {
            continue;
        }
        let Ok(source) = TorrentSource::from_site_enclosure(enclosure) else {
            continue;
        };
        let op_id = PluginOperationId::from_uuid(Uuid::new_v4());
        let plan = build_boost_plan(torrent.clone(), source, dl_id, quota_key);
        let op_ctx = DownloadOpContext {
            operation_id: op_id,
            cause_id: cause_id.to_string(),
            subject: plugin.plugin_id.clone(),
            idempotency_key: format!("boost-cycle:{}:{}", op_id, torrent.enclosure),
            plugin_id: Some(plugin.plugin_id.clone()),
            generation: u64::try_from(plugin.generation).ok(),
            config_revision: u64::try_from(plugin.config_revision).ok(),
            dry_run: false,
        };
        let budget_cap = cfg.budget_bytes;
        let outcome = match &rt {
            Ok(_) => submitter.submit_sync_with_budget_cap(plan, op_ctx, budget_cap),
            Err(_) => {
                let Ok(tmp_rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    break;
                };
                let _enter = tmp_rt.enter();
                submitter.submit_sync_with_budget_cap(plan, op_ctx, budget_cap)
            }
        };
        if !handle_submit_outcome(plugin, enclosure, outcome, summary) {
            break;
        }
    }
}

fn build_boost_plan(
    torrent: Torrent,
    source: TorrentSource,
    dl_id: DownloaderId,
    quota_key: &str,
) -> FrozenDownloadPlan {
    FrozenDownloadPlan {
        torrent,
        source,
        purpose: SubmissionPurpose::Boost,
        downloader_id: Some(dl_id),
        frozen_endpoint: None,
        subscribe_id: None,
        subscribe_generation: None,
        policy_revision: None,
        media_id: None,
        score: None,
        release_override: None,
        force: false,
        save_path: None,
        category: Some("boost".into()),
        tags: vec!["plugin:builtin.boost:auto".into()],
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
        budget_key: Some(quota_key.to_string()),
        plan_digest: None,
    }
}

fn handle_submit_outcome(
    plugin: &PluginRecord,
    enclosure: &str,
    outcome: Result<DownloadOutcome, DownloadSubmitError>,
    summary: &mut BoostCycleSummary,
) -> bool {
    match outcome {
        Ok(DownloadOutcome::Submitted { .. }) => {
            summary.added += 1;
            true
        }
        Ok(DownloadOutcome::Rejected { ref reason, .. }) if reason.contains("quota") => {
            summary.skipped_quota = true;
            false
        }
        Ok(DownloadOutcome::Rejected { ref reason, .. }) => {
            tracing::debug!(plugin_id = %plugin.plugin_id, enclosure, %reason, "boost candidate rejected");
            true
        }
        Ok(DownloadOutcome::Deferred { ref reason, .. }) => {
            tracing::debug!(plugin_id = %plugin.plugin_id, enclosure, %reason, "boost candidate deferred");
            true
        }
        Ok(DownloadOutcome::Unknown { ref reason, .. }) => {
            tracing::warn!(plugin_id = %plugin.plugin_id, enclosure, %reason, "boost candidate outcome unknown");
            true
        }
        Err(DownloadSubmitError::Cancelled) => {
            tracing::info!(plugin_id = %plugin.plugin_id, "boost cycle cancelled during candidate submission");
            false
        }
        Err(err) => {
            tracing::warn!(plugin_id = %plugin.plugin_id, enclosure, error = %err, "boost candidate submission failed");
            true
        }
    }
}
