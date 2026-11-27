use std::collections::{HashMap, HashSet};

use domain::{DownloaderId, PluginOperationId, Torrent};
use downloader::{
    ClientSubmitResult, DownloadOpContext, DownloadOutcome, DownloadSubmitError, Downloader,
    FrozenDownloadPlan, SubmissionPurpose, SubmitOptions, SubmitPayload, TorrentSource,
    magnet_info_hash, ownership_tag,
};
use filter::ScoredTorrent;
use hooks::{Gate, GateContext, PluginError, RescoreBudget, SelectionCandidate, SelectionContext};
use plugin_api::Candidate;

use crate::choose::{candidate_matches_subscribe_with_keywords, is_complete};
use crate::{RunInput, RunOutcome, SubscribeError, SubscribePolicyHooks};

pub struct Added<'a, D: Downloader + ?Sized> {
    pub input: RunInput<'a, D>,
    pub chosen: Vec<ScoredTorrent>,
    /// Failures submitting selected torrents. Successfully submitted torrents
    /// are still returned so the caller can persist them before retrying.
    pub add_errors: Vec<String>,
    /// G-M2: chosen candidates whose existing downloader task is broken.
    pub broken: Vec<ScoredTorrent>,
    /// 被规则组拒绝的候选（score 0；供 wanted 记录拒绝原因）。
    pub rejected: Vec<ScoredTorrent>,
    /// Candidates deferred by policy hooks (cand, until_unix, reason).
    pub deferred: Vec<(ScoredTorrent, i64, String)>,
    /// Pre-submitter defers from `subscribe.select` / `download` gate hooks.
    pub select_deferred: Vec<(ScoredTorrent, i64, String)>,
    /// Candidates with post-send Unknown outcome awaiting reconciliation.
    pub unknown: Vec<ScoredTorrent>,
    /// Resolved downloader_id & info_hash keyed by enclosure.
    pub submitted_meta: HashMap<String, (Option<DownloaderId>, Option<String>)>,
    pub outcome: RunOutcome,
}

impl<'a, D: Downloader + ?Sized> Added<'a, D> {
    /// Candidates deferred specifically by pre-submitter policy/gate hooks
    /// (excluding `download.before` defers, which are already persisted by the submitter).
    pub fn select_deferred(&self) -> &[(ScoredTorrent, i64, String)] {
        &self.select_deferred
    }
}

pub fn admit_and_add<D: Downloader + ?Sized>(
    mut input: RunInput<'_, D>,
) -> Result<Added<'_, D>, SubscribeError> {
    let total_candidates = input.torrents.len();
    let torrents = std::mem::take(&mut input.torrents);
    let admission = match input.wash_filter.filter(|_| input.subscribe.wash_cut) {
        Some(wash) => split_admission(&input, torrents, wash),
        None => filter::admit(torrents, input.filter),
    };
    let mut admitted = filter_admitted_candidates(&input, admission.admitted);
    if let Some(policy_hooks) = input.policy_hooks {
        apply_subscribe_policies(policy_hooks, &input, &mut admitted);
    }
    tracing::info!(
        media = %input.media.title,
        total = total_candidates,
        filter_passed = admitted.len(),
        rejected = admission.rejected.len(),
        "候选种子通过过滤准入"
    );
    let effective_wash_filter = input.wash_filter.or(Some(input.filter));
    let Submission {
        submitted,
        add_errors,
        broken,
        deferred,
        select_deferred,
        unknown,
        submitted_meta,
    } = submit_with_source_fallback(&input, effective_wash_filter, &admitted)?;
    let torrents_added: Vec<(i32, Torrent)> = submitted
        .iter()
        .map(|s| (s.score, s.torrent.clone()))
        .collect();
    let completed = is_complete(input.subscribe, &input.facts);
    Ok(Added {
        outcome: RunOutcome {
            completed,
            facts: input.facts.clone(),
            files: Vec::new(),
            file_sources: Vec::new(),
            removed_paths: Vec::new(),
            transferred_enclosures: Vec::new(),
            rejected_enclosures: broken.iter().map(|s| s.torrent.enclosure.clone()).collect(),
            awaiting_probe_enclosures: Vec::new(),
            collection_errors: Vec::new(),
            submission_errors: add_errors.clone(),
            torrents_added,
            deferred_enclosures: deferred
                .iter()
                .map(|(s, _, _)| s.torrent.enclosure.clone())
                .collect(),
            unknown_enclosures: unknown
                .iter()
                .map(|s| s.torrent.enclosure.clone())
                .collect(),
            submitted_meta: submitted_meta.clone(),
        },
        input,
        chosen: submitted,
        broken,
        add_errors,
        rejected: admission.rejected,
        deferred,
        select_deferred,
        unknown,
        submitted_meta,
    })
}

struct Submission {
    submitted: Vec<ScoredTorrent>,
    add_errors: Vec<String>,
    broken: Vec<ScoredTorrent>,
    deferred: Vec<(ScoredTorrent, i64, String)>,
    select_deferred: Vec<(ScoredTorrent, i64, String)>,
    unknown: Vec<ScoredTorrent>,
    submitted_meta: HashMap<String, (Option<DownloaderId>, Option<String>)>,
}

fn to_plugin_candidate(handle: String, scored: &ScoredTorrent) -> Candidate {
    Candidate {
        handle,
        site_id: scored.torrent.site_id.to_string(),
        provider: "indexer".to_string(),
        generation: 1,
        title: scored.torrent.title.clone(),
        size: scored.torrent.size_bytes,
        seeders: scored.torrent.seeders,
        leechers: scored.torrent.leechers,
        published_at: scored
            .torrent
            .upload_time
            .as_deref()
            .and_then(hooks::parse_candidate_published_at_ms),
        free: Some(scored.torrent.free),
        hr: scored.torrent.hr,
        release: scored.release.resolution.clone(),
        score: Some(scored.score),
        labels: Vec::new(),
        revision: 1,
    }
}

fn filter_admitted_candidates<D: Downloader + ?Sized>(
    input: &RunInput<'_, D>,
    admitted: Vec<ScoredTorrent>,
) -> Vec<ScoredTorrent> {
    admitted
        .into_iter()
        .filter_map(|mut candidate| {
            if let Some(map) = input.absolute {
                match map.remap(&candidate.release) {
                    Ok(Some(mapped)) => {
                        tracing::info!(torrent = %candidate.torrent.title, season = ?mapped.season, episode = ?mapped.episode, "绝对集号映射为季+集");
                        candidate.release = mapped;
                    }
                    Ok(None) => {}
                    Err(crate::absolute::Unmappable) => {
                        tracing::info!(torrent = %candidate.torrent.title, "绝对集号跨季或超出目录，跳过");
                        return None;
                    }
                }
            }
            Some(candidate)
        })
        .filter(|candidate| {
            let m = candidate_matches_subscribe_with_keywords(
                input.subscribe,
                input.media,
                &candidate.release,
                &input.search_keywords,
            );
            if !m {
                tracing::debug!(media = %input.media.title, torrent = %candidate.torrent.title, "候选种子未匹配订阅媒体");
            }
            m
        })
        .collect()
}

fn apply_subscribe_policies<D: Downloader + ?Sized>(
    hooks: &dyn SubscribePolicyHooks,
    input: &RunInput<'_, D>,
    admitted: &mut Vec<ScoredTorrent>,
) {
    if admitted.is_empty() {
        return;
    }
    let scores: Vec<i32> = admitted.iter().map(|s| s.score).collect();
    let mut budget = RescoreBudget::new(&scores);
    let mut cands: Vec<Candidate> = admitted
        .iter()
        .enumerate()
        .map(|(i, s)| to_plugin_candidate(format!("sub:{i}"), s))
        .collect();
    let mut handles: Vec<String> = cands.iter().map(|c| c.handle.clone()).collect();
    hooks.filter_after(input.subscribe.id, &mut cands, &mut handles, &mut budget);
    if !cands.is_empty() {
        hooks.score(
            input.subscribe.id,
            input.media.id,
            &mut cands,
            &mut handles,
            &mut budget,
        );
    }
    let mut by_idx: Vec<Option<ScoredTorrent>> = admitted.drain(..).map(Some).collect();
    for cand in cands {
        if let Some(idx_str) = cand.handle.strip_prefix("sub:")
            && let Ok(idx) = idx_str.parse::<usize>()
            && let Some(mut item) = by_idx.get_mut(idx).and_then(Option::take)
        {
            if let Some(score) = cand.score {
                item.score = score;
            }
            admitted.push(item);
        }
    }
}

enum PolicySelectDecision {
    Selected(Vec<String>),
    Deferred(Vec<(ScoredTorrent, i64, String)>),
}

fn apply_policy_select<D: Downloader + ?Sized>(
    input: &RunInput<'_, D>,
    chosen: &[ScoredTorrent],
    selected: Vec<String>,
) -> Result<PolicySelectDecision, SubscribeError> {
    let Some(policy_hooks) = input.policy_hooks else {
        return Ok(PolicySelectDecision::Selected(selected));
    };
    if selected.is_empty() {
        return Ok(PolicySelectDecision::Selected(selected));
    }
    let mut p_cands: Vec<Candidate> = selected
        .iter()
        .filter_map(|h| h.parse::<usize>().ok().map(|i| (h, &chosen[i])))
        .map(|(h, s)| to_plugin_candidate(h.clone(), s))
        .collect();
    let mut p_admitted = selected.clone();
    match policy_hooks.select(
        input.subscribe.id,
        input.media.id,
        &mut p_cands,
        &mut p_admitted,
    ) {
        Gate::Allow | Gate::Abstain => Ok(PolicySelectDecision::Selected(p_admitted)),
        Gate::Reject(reason) => Err(PluginError::Veto(reason).into()),
        Gate::Defer { until_ms, reason } => {
            let until_unix =
                unix_now().saturating_add(i64::try_from((until_ms / 1000).max(1)).unwrap_or(60));
            let allowed: HashSet<&str> = p_admitted.iter().map(String::as_str).collect();
            let deferred = chosen
                .iter()
                .enumerate()
                .filter(|(i, _)| allowed.contains(i.to_string().as_str()))
                .map(|(_, s)| (s.clone(), until_unix, reason.clone()))
                .collect();
            Ok(PolicySelectDecision::Deferred(deferred))
        }
    }
}

fn check_download_gate(
    pipeline: Option<&hooks::HookPipeline>,
    subscribe_id: domain::SubscribeId,
    media_id: domain::MediaId,
    candidate: SelectionCandidate,
    title: &str,
) -> Result<Option<(Option<i64>, String)>, SubscribeError> {
    let Some(pipeline) = pipeline else {
        return Ok(None);
    };
    match pipeline
        .gate(&GateContext::Download {
            subscribe_id,
            media_id,
            candidate,
        })
        .map_err(PluginError::HookRuntime)?
    {
        Gate::Allow | Gate::Abstain => Ok(None),
        Gate::Reject(reason) => {
            tracing::error!(torrent = %title, error = %reason, "提交种子被钩子拒绝");
            Ok(Some((None, format!("{title}: {reason}"))))
        }
        Gate::Defer { until_ms, reason } => {
            let until_unix =
                unix_now().saturating_add(i64::try_from((until_ms / 1000).max(1)).unwrap_or(60));
            Ok(Some((Some(until_unix), reason)))
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CandidateStep {
    SlotHandled,
    SourceFallback,
    AbortRound,
}

fn build_subscribe_plan_and_ctx<D: Downloader + ?Sized>(
    input: &RunInput<'_, D>,
    scored: &ScoredTorrent,
    source: TorrentSource,
) -> (FrozenDownloadPlan, DownloadOpContext) {
    let op_id = PluginOperationId::new();
    let plan = FrozenDownloadPlan {
        torrent: scored.torrent.clone(),
        source,
        purpose: SubmissionPurpose::Subscribe,
        downloader_id: input.subscribe.downloader_id,
        frozen_endpoint: None,
        subscribe_id: Some(input.subscribe.id),
        subscribe_generation: None,
        policy_revision: None,
        media_id: Some(input.media.id),
        score: Some(scored.score),
        release_override: Some(scored.release.clone()),
        force: false,
        save_path: None,
        category: None,
        tags: vec![format!("cm-sub:{}", input.subscribe.id)],
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
        budget_key: None,
        plan_digest: None,
    };
    let op_ctx = DownloadOpContext {
        operation_id: op_id,
        cause_id: format!("subscribe:{}", input.subscribe.id),
        subject: format!("subscribe:{}", input.subscribe.id),
        idempotency_key: format!("sub:{}:{op_id}", input.subscribe.id),
        plugin_id: None,
        generation: None,
        config_revision: None,
        dry_run: false,
    };
    (plan, op_ctx)
}

fn submit_via_submitter<D: Downloader + ?Sized>(
    input: &RunInput<'_, D>,
    submitter: &dyn crate::SyncDownloadSubmitter,
    scored: ScoredTorrent,
    sub: &mut Submission,
) -> CandidateStep {
    let source = match TorrentSource::from_site_enclosure(&scored.torrent.enclosure) {
        Ok(s) => s,
        Err(err) => {
            sub.add_errors
                .push(format!("{}: {err}", scored.torrent.title));
            return CandidateStep::SlotHandled;
        }
    };
    let (plan, op_ctx) = build_subscribe_plan_and_ctx(input, &scored, source);
    match submitter.submit_sync(plan, op_ctx) {
        Ok(DownloadOutcome::Submitted {
            downloader_id,
            info_hash,
            ..
        }) => {
            sub.submitted_meta
                .insert(scored.torrent.enclosure.clone(), (downloader_id, info_hash));
            sub.submitted.push(scored);
            CandidateStep::SlotHandled
        }
        Ok(DownloadOutcome::Rejected {
            broken_task_hash: Some(hash),
            ..
        }) => {
            tracing::warn!(torrent = %scored.torrent.title, %hash, "下载器中已有任务处于错误/缺文件状态，拒绝该来源以便换源");
            sub.broken.push(scored);
            CandidateStep::SourceFallback
        }
        Ok(DownloadOutcome::Rejected { reason, .. }) => {
            sub.add_errors
                .push(format!("{}: {reason}", scored.torrent.title));
            CandidateStep::SlotHandled
        }
        Ok(DownloadOutcome::Deferred {
            until_unix, reason, ..
        }) => {
            sub.deferred.push((scored, until_unix, reason));
            CandidateStep::SlotHandled
        }
        Ok(DownloadOutcome::Unknown { downloader_id, .. }) => {
            sub.submitted_meta
                .insert(scored.torrent.enclosure.clone(), (downloader_id, None));
            sub.unknown.push(scored);
            CandidateStep::SlotHandled
        }
        Err(DownloadSubmitError::Downloader(
            ref err @ (downloader::DownloaderError::ExistingTaskBroken(_)
            | downloader::DownloaderError::SourceRejected(_)),
        )) => {
            tracing::warn!(torrent = %scored.torrent.title, error = %err, "候选源不可用，排除并自动尝试下一顺位");
            sub.broken.push(scored);
            CandidateStep::SourceFallback
        }
        Err(DownloadSubmitError::Fetch(
            ref err @ (downloader::TorrentFetchError::HttpStatus {
                status: 404 | 410, ..
            }
            | downloader::TorrentFetchError::Parse(_)
            | downloader::TorrentFetchError::InvalidMagnet),
        )) => {
            tracing::warn!(torrent = %scored.torrent.title, error = %err, "候选种子源无效或已失效，排除并自动尝试下一顺位");
            sub.broken.push(scored);
            CandidateStep::SourceFallback
        }
        Err(DownloadSubmitError::InvalidMetainfo(ref err)) => {
            tracing::warn!(torrent = %scored.torrent.title, error = %err, "候选种子元信息无效，排除并自动尝试下一顺位");
            sub.broken.push(scored);
            CandidateStep::SourceFallback
        }
        Err(DownloadSubmitError::Fetch(err)) => {
            tracing::warn!(torrent = %scored.torrent.title, error = %err, "拉取候选种子遇到暂时性错误，保留槽位待后续重试");
            sub.add_errors
                .push(format!("{}: {err}", scored.torrent.title));
            CandidateStep::SlotHandled
        }
        Err(DownloadSubmitError::Cancelled) => {
            sub.add_errors
                .push(format!("{}: cancelled", scored.torrent.title));
            CandidateStep::AbortRound
        }
        Err(error) => {
            tracing::error!(torrent = %scored.torrent.title, error = %error, "提交种子到下载器失败");
            sub.add_errors
                .push(format!("{}: {error}", scored.torrent.title));
            CandidateStep::SlotHandled
        }
    }
}

fn submit_via_legacy_magnet<D: Downloader + ?Sized>(
    input: &RunInput<'_, D>,
    hash: String,
    scored: ScoredTorrent,
    sub: &mut Submission,
) -> CandidateStep {
    let payload = SubmitPayload::Magnet {
        uri: scored.torrent.enclosure.clone(),
        info_hash: hash,
    };
    let opts = SubmitOptions {
        ownership_tag: ownership_tag(&scored.torrent.enclosure),
        extra_tags: vec![format!("cm-sub:{}", input.subscribe.id)],
        ..Default::default()
    };
    match input.downloader.submit_verified(&payload, &opts) {
        Ok(ClientSubmitResult::Added { info_hash, .. }) => {
            let _ = input
                .downloader
                .remember_identity(&scored.torrent, &info_hash);
            sub.submitted_meta.insert(
                scored.torrent.enclosure.clone(),
                (input.subscribe.downloader_id, Some(info_hash)),
            );
            sub.submitted.push(scored);
            CandidateStep::SlotHandled
        }
        Ok(ClientSubmitResult::ExistingBroken { info_hash: hash }) => {
            tracing::warn!(torrent = %scored.torrent.title, %hash, "下载器中已有任务处于错误/缺文件状态，拒绝该来源以便换源");
            sub.broken.push(scored);
            CandidateStep::SourceFallback
        }
        Ok(ClientSubmitResult::ForeignDuplicate { .. }) => {
            sub.add_errors
                .push(format!("{}: foreign duplicate", scored.torrent.title));
            CandidateStep::SlotHandled
        }
        Ok(ClientSubmitResult::UnknownOutcome { .. }) => {
            sub.unknown.push(scored);
            CandidateStep::SlotHandled
        }
        Err(
            ref err @ (downloader::DownloaderError::ExistingTaskBroken(_)
            | downloader::DownloaderError::SourceRejected(_)),
        ) => {
            tracing::warn!(torrent = %scored.torrent.title, error = %err, "候选源不可用，排除并自动尝试下一顺位");
            sub.broken.push(scored);
            CandidateStep::SourceFallback
        }
        Err(error) => {
            tracing::error!(torrent = %scored.torrent.title, error = %error, "提交种子到下载器失败");
            sub.add_errors
                .push(format!("{}: {error}", scored.torrent.title));
            CandidateStep::SlotHandled
        }
    }
}

fn submit_with_source_fallback<D: Downloader + ?Sized>(
    input: &RunInput<'_, D>,
    effective_wash_filter: Option<&domain::Filter>,
    admitted: &[ScoredTorrent],
) -> Result<Submission, SubscribeError> {
    const MAX_SOURCE_FALLBACKS: usize = 8;
    let mut excluded_enclosures = HashSet::new();
    let mut blocked_releases = Vec::new();
    let mut fallbacks_used = 0usize;
    let mut sub = Submission {
        submitted: Vec::new(),
        add_errors: Vec::new(),
        broken: Vec::new(),
        deferred: Vec::new(),
        select_deferred: Vec::new(),
        unknown: Vec::new(),
        submitted_meta: HashMap::new(),
    };

    loop {
        let remaining: Vec<ScoredTorrent> = admitted
            .iter()
            .filter(|c| !excluded_enclosures.contains(&c.torrent.enclosure))
            .cloned()
            .collect();
        if remaining.is_empty() {
            break;
        }
        let working_facts = crate::facts_with_pending(
            input.facts.clone(),
            &blocked_releases,
            input.subscribe,
            input.media,
        );
        let chosen = crate::choose::choose_with_policy(
            input.subscribe,
            effective_wash_filter,
            &remaining,
            &working_facts,
            input.overlap,
        );
        if chosen.is_empty() {
            break;
        }
        for c in &chosen {
            excluded_enclosures.insert(c.torrent.enclosure.clone());
        }
        let need_retry = run_selected_batch(
            input,
            chosen,
            &mut sub,
            &mut blocked_releases,
            &mut fallbacks_used,
            MAX_SOURCE_FALLBACKS,
        )?;
        if !need_retry {
            break;
        }
    }
    Ok(sub)
}

fn run_selected_batch<D: Downloader + ?Sized>(
    input: &RunInput<'_, D>,
    chosen: Vec<ScoredTorrent>,
    sub: &mut Submission,
    blocked_releases: &mut Vec<domain::Release>,
    fallbacks_used: &mut usize,
    max_fallbacks: usize,
) -> Result<bool, SubscribeError> {
    let candidates: Vec<_> = chosen
        .iter()
        .enumerate()
        .map(|(i, scored)| SelectionCandidate {
            handle: i.to_string(),
            site_id: scored.torrent.site_id,
            title: scored.torrent.title.clone(),
            score: scored.score,
        })
        .collect();
    let selected: Vec<String> = if let Some(pipeline) = input.hooks {
        pipeline
            .select(SelectionContext {
                subscribe_id: input.subscribe.id,
                media_id: input.media.id,
                candidates: candidates.clone(),
            })
            .map_err(PluginError::HookRuntime)?
    } else {
        (0..chosen.len()).map(|i| i.to_string()).collect()
    };
    let selected = match apply_policy_select(input, &chosen, selected)? {
        PolicySelectDecision::Selected(s) => s,
        PolicySelectDecision::Deferred(deferred) => {
            for item in deferred {
                blocked_releases.push(item.0.release.clone());
                sub.select_deferred.push(item.clone());
                sub.deferred.push(item);
            }
            return Ok(false);
        }
    };

    let mut chosen: Vec<_> = chosen.into_iter().map(Some).collect();
    let mut need_retry = false;
    for handle in selected {
        let Some(index) = handle.parse::<usize>().ok() else {
            continue;
        };
        let Some(scored) = chosen.get_mut(index).and_then(Option::take) else {
            continue;
        };
        let cand_release = scored.release.clone();
        tracing::info!(
            media = %input.media.title,
            torrent = %scored.torrent.title,
            score = scored.score,
            "将选中的种子派发给下载器"
        );
        if let Some((until_opt, reason)) = check_download_gate(
            input.hooks,
            input.subscribe.id,
            input.media.id,
            candidates[index].clone(),
            &scored.torrent.title,
        )? {
            blocked_releases.push(cand_release);
            match until_opt {
                Some(until_unix) => {
                    let entry = (scored, until_unix, reason);
                    sub.select_deferred.push(entry.clone());
                    sub.deferred.push(entry);
                }
                None => sub.add_errors.push(reason),
            }
            continue;
        }
        let step = if let Some(submitter) = input.submitter {
            submit_via_submitter(input, submitter, scored, sub)
        } else if let Some(hash) = magnet_info_hash(&scored.torrent.enclosure) {
            submit_via_legacy_magnet(input, hash, scored, sub)
        } else {
            sub.add_errors.push(format!(
                "{}: download submitter required for HTTP torrent",
                scored.torrent.title
            ));
            CandidateStep::SlotHandled
        };
        match step {
            CandidateStep::SlotHandled => blocked_releases.push(cand_release),
            CandidateStep::SourceFallback => {
                if *fallbacks_used < max_fallbacks {
                    *fallbacks_used += 1;
                    need_retry = true;
                }
            }
            CandidateStep::AbortRound => return Ok(false),
        }
    }
    Ok(need_retry)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

pub(crate) fn gate(
    pipeline: Option<&hooks::HookPipeline>,
    context: GateContext,
) -> Result<(), SubscribeError> {
    if let Some(pipeline) = pipeline {
        match pipeline.gate(&context).map_err(PluginError::HookRuntime)? {
            Gate::Reject(reason) | Gate::Defer { reason, .. } => {
                return Err(PluginError::Veto(reason).into());
            }
            Gate::Allow | Gate::Abstain => {}
        }
    }
    Ok(())
}

/// M3: with wash-cut, candidates that fill at least one empty slot are admitted
/// by the main Filter; candidates touching owned slots by the wash Filter. A
/// candidate admitted by both keeps the main-filter entry when it fills a gap.
fn split_admission<D: Downloader + ?Sized>(
    input: &RunInput<'_, D>,
    torrents: Vec<Torrent>,
    wash: &domain::Filter,
) -> filter::AdmitOutcome {
    let fills_gap = |c: &ScoredTorrent| {
        let release = input
            .absolute
            .map_or_else(|| c.release.clone(), |m| m.apply(&c.release));
        crate::choose::covered_slots(input.subscribe, &release)
            .iter()
            .any(|(s, e)| input.facts.get(*s, *e).is_none())
    };
    let touches_owned = |c: &ScoredTorrent| {
        let release = input
            .absolute
            .map_or_else(|| c.release.clone(), |m| m.apply(&c.release));
        crate::choose::covered_slots(input.subscribe, &release)
            .iter()
            .any(|(s, e)| input.facts.get(*s, *e).is_some())
    };
    let main = filter::admit(torrents.clone(), input.filter);
    let washed = filter::admit(torrents, wash);
    let mut outcome = filter::AdmitOutcome::default();
    for candidate in main.admitted.into_iter().filter(|c| fills_gap(c)) {
        outcome.admitted.push(candidate);
    }
    for candidate in washed.admitted.into_iter().filter(|c| touches_owned(c)) {
        if !outcome
            .admitted
            .iter()
            .any(|c| c.torrent.enclosure == candidate.torrent.enclosure)
        {
            outcome.admitted.push(candidate);
        }
    }
    for candidate in washed.rejected {
        if !outcome
            .admitted
            .iter()
            .any(|c| c.torrent.enclosure == candidate.torrent.enclosure)
        {
            outcome.rejected.push(candidate);
        }
    }
    outcome
}
