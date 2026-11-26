use domain::Torrent;
use downloader::Downloader;
use filter::ScoredTorrent;
use hooks::{HookEvent, Step};

use crate::choose::{candidate_matches_subscribe_with_keywords, is_complete};
use crate::{RunInput, RunOutcome, SubscribeError};

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
    pub outcome: RunOutcome,
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
    let admitted = admission
        .admitted
        .into_iter()
        .filter_map(|mut candidate| {
            // G-H2b: absolute episode numbers → catalog season + episode.
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
        .collect::<Vec<_>>();
    tracing::info!(
        media = %input.media.title,
        total = total_candidates,
        filter_passed = admitted.len(),
        rejected = admission.rejected.len(),
        "候选种子通过过滤准入"
    );
    let effective_wash_filter = input.wash_filter.or(Some(input.filter));
    emit(input.hooks, Step::ChooseTorrent)?;
    let FallbackOutcome {
        submitted,
        add_errors,
        broken,
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
        },
        input,
        chosen: submitted,
        broken,
        add_errors,
        rejected: admission.rejected,
    })
}

struct FallbackOutcome {
    submitted: Vec<ScoredTorrent>,
    add_errors: Vec<String>,
    broken: Vec<ScoredTorrent>,
}

/// 逐轮选种并投递下载器；若选中候选因源端 `.torrent` 拉取失败或任务已损坏被拒，
/// 则在同一轮内剔除该候选并顺延补位下一候选（最多 `MAX_SOURCE_FALLBACKS` 次）。
fn submit_with_source_fallback<D: Downloader + ?Sized>(
    input: &RunInput<'_, D>,
    effective_wash_filter: Option<&domain::Filter>,
    admitted: &[ScoredTorrent],
) -> Result<FallbackOutcome, SubscribeError> {
    const MAX_SOURCE_FALLBACKS: usize = 8;
    let mut excluded_enclosures = std::collections::HashSet::new();
    let mut blocked_releases = Vec::new();
    let mut submitted = Vec::new();
    let mut add_errors = Vec::new();
    let mut broken = Vec::new();
    let mut fallbacks_used = 0usize;

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
        let mut need_retry = false;
        for scored in chosen {
            excluded_enclosures.insert(scored.torrent.enclosure.clone());
            tracing::info!(
                media = %input.media.title,
                torrent = %scored.torrent.title,
                score = scored.score,
                "将选中的种子派发给下载器"
            );
            let result = emit(input.hooks, Step::AddDownload)
                .and_then(|()| input.downloader.add(&scored.torrent).map_err(Into::into));
            match result {
                Ok(()) => {
                    blocked_releases.push(scored.release.clone());
                    submitted.push(scored);
                }
                Err(SubscribeError::Downloader(
                    ref err @ (downloader::DownloaderError::ExistingTaskBroken(_)
                    | downloader::DownloaderError::SourceRejected(_)),
                )) => {
                    tracing::warn!(torrent = %scored.torrent.title, error = %err, "候选源不可用，排除并自动尝试下一顺位");
                    broken.push(scored);
                    if fallbacks_used < MAX_SOURCE_FALLBACKS {
                        fallbacks_used += 1;
                        need_retry = true;
                    }
                }
                Err(error) => {
                    tracing::error!(torrent = %scored.torrent.title, error = %error, "提交种子到下载器失败");
                    blocked_releases.push(scored.release.clone());
                    add_errors.push(format!("{}: {error}", scored.torrent.title));
                }
            }
        }
        if !need_retry {
            break;
        }
    }
    Ok(FallbackOutcome {
        submitted,
        add_errors,
        broken,
    })
}

fn emit(bus: Option<&hooks::Bus>, step: Step) -> Result<(), SubscribeError> {
    if let Some(bus) = bus {
        bus.emit(&HookEvent { step })?;
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
