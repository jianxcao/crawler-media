use std::collections::{HashMap, HashSet};

use domain::{DownloaderId, PluginOperationId, SiteId, Torrent};
use downloader::{
    DownloadOpContext, DownloadOutcome, DownloadSubmitError, FrozenDownloadPlan, SubmissionPurpose,
    TorrentSource,
};
use store::plugins::PluginRecord;
use uuid::Uuid;

use super::config::{BoostConfig, EffectiveBoostRule};
use super::cycle::{
    AuthorizedBoostSite, BoostCycleSummary, is_live_plugin_active_at, resolve_target_downloader,
};
use super::cycle_filter;
use crate::download_service::ApiDownloadService;
use crate::management::ApiState;

pub(super) struct DiscoveredCandidate {
    pub site_id: SiteId,
    pub torrent: Torrent,
}

pub(super) fn discover_candidates(
    state: &ApiState,
    cfg: &BoostConfig,
    sites: &[AuthorizedBoostSite],
    now: i64,
) -> Vec<DiscoveredCandidate> {
    if sites.is_empty() {
        return Vec::new();
    }
    let snapshot = state.indexer.snapshot();
    let mut hits = Vec::new();
    for entry in sites {
        if !cfg.is_site_enabled(entry.site.id) {
            continue;
        }
        let rule = cfg.effective_for_site(Some(entry.site.id));
        if !cycle_filter::is_site_eligible(state, &entry.site, &rule, now) {
            continue;
        }
        let profile = snapshot.profile(&entry.site.profile_id);
        let cat_mapping = profile.as_ref().and_then(|p| p.categories.as_ref());
        let site_slice = std::slice::from_ref(&entry.site);
        let mut raw_hits = Vec::new();
        if entry.can_search {
            let outcome = state.indexer.search_page_categories_with_snapshot(
                &snapshot,
                site_slice,
                "",
                1,
                &rule.categories,
            );
            raw_hits = outcome.torrents;
        }
        if raw_hits.is_empty() && entry.can_rss {
            raw_hits = state
                .indexer
                .rss_with_snapshot(&snapshot, site_slice)
                .torrents;
        }
        for torrent in raw_hits {
            if cycle_filter::candidate_matches(&torrent, &entry.site, &rule, cat_mapping, now) {
                hits.push(DiscoveredCandidate {
                    site_id: entry.site.id,
                    torrent,
                });
            }
        }
    }
    hits
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
    cfg: &BoostConfig,
    dl_id: DownloaderId,
    quota_key: &str,
    cause_id: &str,
    candidates: Vec<DiscoveredCandidate>,
    mut seen_enclosures: HashSet<String>,
    tracked_hashes: &HashSet<String>,
    round_cap: usize,
    exclude_self_hook: bool,
    summary: &mut BoostCycleSummary,
) {
    let mut submitter = ApiDownloadService::new(state.clone());
    if exclude_self_hook {
        submitter = submitter.with_fence(crate::download_service::SubmissionFence {
            exclude_hook_plugin: Some(plugin.plugin_id.clone()),
            ..Default::default()
        });
    }
    let rt = tokio::runtime::Handle::try_current();
    let mut added_per_site: HashMap<SiteId, usize> = HashMap::new();
    for cand in candidates {
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
            .observe_site(cand.torrent.site_id)
            .ok()
            .flatten()
            .is_some_and(|obs| obs.site().enabled)
        {
            continue;
        }
        let rule = cfg.effective_for_site(Some(cand.site_id));
        if *added_per_site.get(&cand.site_id).unwrap_or(&0) >= rule.max_adds_per_round {
            continue;
        }
        let enclosure = cand.torrent.enclosure.trim().to_string();
        if enclosure.is_empty() || !seen_enclosures.insert(enclosure.clone()) {
            continue;
        }
        if let Some(mag_hash) = downloader::magnet_info_hash(&enclosure)
            && tracked_hashes.contains(&mag_hash.to_ascii_lowercase())
        {
            continue;
        }
        let Ok(source) = TorrentSource::from_site_enclosure(&enclosure) else {
            continue;
        };
        let op_id = PluginOperationId::from_uuid(Uuid::new_v4());
        let plan = build_boost_plan(
            cand.torrent,
            source,
            dl_id,
            quota_key,
            &plugin.plugin_id,
            &rule,
        );
        let op_ctx = DownloadOpContext {
            operation_id: op_id,
            cause_id: cause_id.to_string(),
            subject: plugin.plugin_id.clone(),
            idempotency_key: format!("boost-cycle:{op_id}:{enclosure}"),
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
        match handle_submit_outcome(plugin, &enclosure, outcome, summary) {
            SubmitFollowUp::Admitted => {
                *added_per_site.entry(cand.site_id).or_default() += 1;
            }
            SubmitFollowUp::Continue => {}
            SubmitFollowUp::Stop => break,
        }
    }
}

fn build_boost_plan(
    torrent: Torrent,
    source: TorrentSource,
    dl_id: DownloaderId,
    quota_key: &str,
    plugin_id: &str,
    rule: &EffectiveBoostRule,
) -> FrozenDownloadPlan {
    let mut tags = vec![format!("plugin:{plugin_id}:auto")];
    for tag in &rule.tags {
        if !tags.contains(tag) {
            tags.push(tag.clone());
        }
    }
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
        save_path: rule.relative_directory.clone(),
        category: rule.category.clone().or_else(|| Some("boost".into())),
        tags,
        paused: false,
        upload_limit: rule.upload_limit_bytes,
        download_limit: rule.download_limit_bytes,
        sequential: rule.sequential,
        first_last_piece: rule.first_last_piece,
        budget_key: Some(quota_key.to_string()),
        plan_digest: None,
    }
}

enum SubmitFollowUp {
    Admitted,
    Continue,
    Stop,
}

fn handle_submit_outcome(
    plugin: &PluginRecord,
    enclosure: &str,
    outcome: Result<DownloadOutcome, DownloadSubmitError>,
    summary: &mut BoostCycleSummary,
) -> SubmitFollowUp {
    match outcome {
        Ok(DownloadOutcome::Submitted { .. }) => {
            summary.added += 1;
            SubmitFollowUp::Admitted
        }
        Ok(DownloadOutcome::Rejected { ref reason, .. }) if reason.contains("quota") => {
            summary.skipped_quota = true;
            SubmitFollowUp::Stop
        }
        Ok(DownloadOutcome::Rejected { ref reason, .. }) => {
            tracing::debug!(plugin_id = %plugin.plugin_id, enclosure, %reason, "boost candidate rejected");
            SubmitFollowUp::Continue
        }
        Ok(DownloadOutcome::Deferred { ref reason, .. }) => {
            tracing::debug!(plugin_id = %plugin.plugin_id, enclosure, %reason, "boost candidate deferred");
            SubmitFollowUp::Continue
        }
        Ok(DownloadOutcome::Unknown { ref reason, .. }) => {
            tracing::warn!(plugin_id = %plugin.plugin_id, enclosure, %reason, "boost candidate outcome unknown");
            SubmitFollowUp::Continue
        }
        Err(DownloadSubmitError::Cancelled) => {
            tracing::info!(plugin_id = %plugin.plugin_id, "boost cycle cancelled during candidate submission");
            SubmitFollowUp::Stop
        }
        Err(err) => {
            tracing::warn!(plugin_id = %plugin.plugin_id, enclosure, error = %err, "boost candidate submission failed");
            SubmitFollowUp::Continue
        }
    }
}
