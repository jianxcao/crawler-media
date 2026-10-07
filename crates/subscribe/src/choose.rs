use std::collections::HashMap;

use domain::{Coverage, Media, MediaKind, Release, Subscribe};
use filter::ScoredTorrent;

use crate::facts::SubscribeFacts;

pub fn choose(
    subscribe: &Subscribe,
    wash_filter: Option<&domain::Filter>,
    admitted: &[ScoredTorrent],
    facts: &SubscribeFacts,
) -> Vec<ScoredTorrent> {
    let mut chosen = Vec::new();
    if subscribe.full_season_pack {
        if let Some(pack) = admitted
            .iter()
            .filter(|c| is_pack(&c.release, subscribe))
            .filter(|c| should_take(subscribe, wash_filter, facts, c))
            .max_by_key(|c| c.score)
        {
            chosen.push(pack.clone());
        }
        return chosen;
    }

    match subscribe.coverage {
        Coverage::Movie => {
            if let Some(best) = admitted
                .iter()
                .filter(|c| should_take(subscribe, wash_filter, facts, c))
                .max_by_key(|c| c.score)
            {
                chosen.push(best.clone());
            }
        }
        Coverage::Tv { .. } => {
            let mut best_by_ep: HashMap<(u32, u32), &ScoredTorrent> = HashMap::new();
            for candidate in admitted
                .iter()
                .filter(|c| should_take(subscribe, wash_filter, facts, c))
            {
                for (season, ep) in candidate.release.covered_episodes() {
                    best_by_ep
                        .entry((season, ep))
                        .and_modify(|current| {
                            if candidate.score > current.score {
                                *current = candidate;
                            }
                        })
                        .or_insert(candidate);
                }
            }
            let mut seen = Vec::new();
            for candidate in best_by_ep.into_values() {
                if seen
                    .iter()
                    .any(|c: &&ScoredTorrent| c.torrent.enclosure == candidate.torrent.enclosure)
                {
                    continue;
                }
                seen.push(candidate);
                chosen.push(candidate.clone());
            }
        }
    }
    chosen
}

fn should_take(
    subscribe: &Subscribe,
    wash_filter: Option<&domain::Filter>,
    facts: &SubscribeFacts,
    candidate: &ScoredTorrent,
) -> bool {
    let slots = covered_slots(subscribe, &candidate.release);
    should_replace_slots(subscribe, wash_filter, facts, candidate, &slots)
}

/// 逐槽位判定候选是否应替换已有事实。choose() 用它做选择；collect.rs 用它
/// 决定事实是否无条件覆盖——两个阶段必须共用同一把尺。
pub(crate) fn should_replace_slots(
    subscribe: &Subscribe,
    wash_filter: Option<&domain::Filter>,
    facts: &SubscribeFacts,
    candidate: &ScoredTorrent,
    slots: &[(Option<u32>, Option<u32>)],
) -> bool {
    if slots.is_empty() {
        return false;
    }
    slots
        .iter()
        .any(|(season, episode)| match facts.get(*season, *episode) {
            None => true,
            Some(existing) => {
                if !subscribe.wash_cut {
                    return false;
                }
                // T5: cutoff 与 ladder 排序分离：
                // 只有 target 显式声明的维度全部满足时才算已达标停止升级；
                // UpgradeLadder 仅用于比对候选优劣，不替代达标判断。
                if let Some(target_value) = wash_target_value(wash_filter) {
                    let target_release = release_from_target_value(&target_value);
                    if target_release.resolution.is_none()
                        && target_release.source.is_none()
                        && target_release.codec.is_none()
                        && target_release.hdr.is_none()
                    {
                        tracing::error!(target_value = %target_value, "WashTarget 配置非法或无法解析有效维度");
                    } else {
                        let existing_quality = existing.path.as_deref().and_then(|path| {
                            facts.quality(path).cloned().or_else(|| {
                                std::path::Path::new(path)
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .map(release::parse)
                                    .filter(|r| r.resolution.is_some() || r.source.is_some())
                            })
                        });
                        if let Some(existing_q) = existing_quality {
                            if target_reached(&existing_q, &target_release) {
                                return false;
                            }
                        }
                    }
                    // 无法确定现有质量时继续尝试升级
                }
                // 配置了 UpgradeLadder → 按维度逐项比较新老版本；否则分数更高才换。
                match ladder_for(wash_filter) {
                    Some(ladder) => {
                        let old_quality = existing.path.as_deref().and_then(|path| {
                            facts.quality(path).cloned().or_else(|| {
                                std::path::Path::new(path)
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .map(release::parse)
                                    .filter(|r| {
                                        r.resolution.is_some()
                                            || r.source.is_some()
                                            || r.codec.is_some()
                                            || r.hdr.is_some()
                                    })
                            })
                        });
                        match old_quality {
                            Some(old) => {
                                ladder_compare(&candidate.release, &old, &ladder)
                                    == std::cmp::Ordering::Greater
                            }
                            None => candidate.score > existing.score,
                        }
                    }
                    None => candidate.score > existing.score,
                }
            }
        })
}

/// 返回 WashTarget atom 的 value 字符串（如 "2160p web-dl"）。
/// D06: 不再返回 priority 数字，而是返回目标质量描述，以便做维度比较。
fn wash_target_value(wash_filter: Option<&domain::Filter>) -> Option<String> {
    let filter = wash_filter?;
    filter.atoms.iter().find_map(|a| match &a.rule {
        domain::AtomRule::WashTarget(v) => Some(v.clone()),
        _ => None,
    })
}

/// 把 WashTarget value 字符串 parse 成伪 Release，用于维度比较。
fn release_from_target_value(value: &str) -> domain::Release {
    release::parse(value)
}

/// 检查 owned 是否已达到 target 声明的截止目标。
/// 只有 target 显式声明的维度参与比较（要求非空）；
/// 对于每个声明的维度，owned 必须已知且等级 >= 目标的等级。
pub fn target_reached(owned: &Release, target: &Release) -> bool {
    let mut specified = 0;

    if let Some(target_res) = &target.resolution {
        specified += 1;
        let Some(owned_res) = &owned.resolution else {
            return false;
        };
        if resolution_level(owned_res) < resolution_level(target_res) {
            return false;
        }
    }

    if let Some(target_src) = &target.source {
        specified += 1;
        let Some(owned_src) = &owned.source else {
            return false;
        };
        if source_level(owned_src) < source_level(target_src) {
            return false;
        }
    }

    if let Some(target_codec) = &target.codec {
        specified += 1;
        let Some(owned_codec) = &owned.codec else {
            return false;
        };
        if codec_level(owned_codec) < codec_level(target_codec) {
            return false;
        }
    }

    if let Some(target_hdr) = &target.hdr {
        specified += 1;
        let Some(owned_hdr) = &owned.hdr else {
            return false;
        };
        if hdr_level(owned_hdr) < hdr_level(target_hdr) {
            return false;
        }
    }

    // specified target dimensions must be nonempty
    if specified == 0 {
        return false;
    }

    true
}

/// Ordered Wash-cut comparison dimensions configured on the Filter.
fn ladder_for(wash_filter: Option<&domain::Filter>) -> Option<Vec<String>> {
    let filter = wash_filter?;
    filter.atoms.iter().find_map(|atom| match &atom.rule {
        domain::AtomRule::UpgradeLadder(raw) => Some(
            raw.split(',')
                .map(|dim| dim.trim().to_string())
                .filter(|dim| !dim.is_empty())
                .collect::<Vec<_>>(),
        ),
        _ => None,
    })
}

/// 按维度顺序比较新老版本：第一个分出胜负的维度决定结果，全部相等 → Equal。
fn ladder_compare(new: &Release, old: &Release, ladder: &[String]) -> std::cmp::Ordering {
    for dim in ladder {
        let ordering = match dim.as_str() {
            "resolution" => cmp_level(&new.resolution, &old.resolution, |v| resolution_level(v)),
            "source" => cmp_level(&new.source, &old.source, |v| source_level(v)),
            "codec" => cmp_level(&new.codec, &old.codec, |v| codec_level(v)),
            "hdr" => cmp_level(&new.hdr, &old.hdr, |v| hdr_level(v)),
            _ => std::cmp::Ordering::Equal,
        };
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    std::cmp::Ordering::Equal
}

fn cmp_level(
    new: &Option<String>,
    old: &Option<String>,
    level: impl Fn(&str) -> i32,
) -> std::cmp::Ordering {
    let new_level = new.as_deref().map(&level).unwrap_or(0);
    let old_level = old.as_deref().map(&level).unwrap_or(0);
    new_level.cmp(&old_level)
}

fn resolution_level(value: &str) -> i32 {
    match value.to_ascii_lowercase().as_str() {
        "2160p" | "4k" | "uhd" => 4,
        "1080p" | "1080i" => 3,
        "720p" => 2,
        "480p" | "576p" => 1,
        _ => 0,
    }
}

fn source_level(value: &str) -> i32 {
    match value.to_ascii_lowercase().as_str() {
        "remux" => 6,
        "bluray" | "blu-ray" => 5,
        "web-dl" | "webdl" => 4,
        "webrip" => 3,
        "hdtv" => 2,
        "dvdrip" | "cam" | "ts" => 1,
        _ => 0,
    }
}

fn codec_level(value: &str) -> i32 {
    match value.to_ascii_lowercase().as_str() {
        "av1" => 4,
        "hevc" | "x265" | "h.265" => 3,
        "x264" | "h.264" | "avc" => 2,
        _ => 0,
    }
}

fn hdr_level(value: &str) -> i32 {
    match value.to_ascii_lowercase().as_str() {
        "dv" | "dovi" | "dolbyvision" => 3,
        "hdr10+" => 2,
        "hdr10" | "hdr" => 1,
        _ => 0,
    }
}

pub(crate) fn covered_slots(
    subscribe: &Subscribe,
    release: &Release,
) -> Vec<(Option<u32>, Option<u32>)> {
    match subscribe.coverage {
        Coverage::Movie => vec![(None, None)],
        Coverage::Tv {
            season,
            episode_from,
            episode_to,
        } => {
            let Some(rel_season) = release.season else {
                return Vec::new();
            };
            if rel_season != season {
                return Vec::new();
            }
            let (from, to) = match release.episode_span() {
                Some(span) => span,
                None if subscribe.full_season_pack => {
                    let pack_to = episode_to.unwrap_or(episode_from);
                    let capped =
                        episode_from.saturating_add(Coverage::MAX_EPISODES.saturating_sub(1));
                    (episode_from, pack_to.min(capped))
                }
                None => return Vec::new(),
            };
            let window_to =
                episode_to.unwrap_or(from.saturating_add(Coverage::MAX_EPISODES.saturating_sub(1)));
            (from.max(episode_from)..=to.min(window_to))
                .map(|ep| (Some(season), Some(ep)))
                .collect()
        }
    }
}

/// Candidates from automatic search, manual delivery, and persisted pending
/// rows must share the Subscribe's Media identity and coverage.
pub fn candidate_matches_subscribe(
    subscribe: &Subscribe,
    media: &Media,
    release: &Release,
) -> bool {
    let keywords = short_title_keyword(media);
    candidate_matches_subscribe_with_keywords(subscribe, media, release, &keywords)
}

fn short_title_keyword(media: &Media) -> Vec<String> {
    let short = media
        .title
        .split(['：', ':', '（', '(', '「', '『'])
        .next()
        .unwrap_or(&media.title)
        .trim();
    if short.is_empty() || short == media.title {
        Vec::new()
    } else {
        vec![short.to_string()]
    }
}

pub(crate) fn candidate_matches_subscribe_with_keywords(
    subscribe: &Subscribe,
    media: &Media,
    release: &Release,
    search_keywords: &[String],
) -> bool {
    if subscribe.media_id != media.id || !matches_media(media, release, search_keywords) {
        return false;
    }
    release_matches_subscribe(subscribe, media, release)
}

fn release_matches_subscribe(subscribe: &Subscribe, media: &Media, release: &Release) -> bool {
    if let (Some(want), Some(got)) = (media.year, release.year) {
        if want != got {
            return false;
        }
    }
    match (&subscribe.coverage, media.kind) {
        (Coverage::Movie, MediaKind::Movie) => {
            release.season.is_none() && release.episode.is_none() && release.episode_to.is_none()
        }
        (Coverage::Tv { .. }, MediaKind::Tv) => !covered_slots(subscribe, release).is_empty(),
        _ => false,
    }
}

fn is_pack(release: &Release, subscribe: &Subscribe) -> bool {
    let Coverage::Tv {
        season,
        episode_from,
        episode_to,
    } = subscribe.coverage
    else {
        return false;
    };
    let Some(episode_to) = episode_to else {
        return false;
    };
    if release.season != Some(season) {
        return false;
    }
    // 明确标注了范围 E01-E10 的整季包，或者省略集号的 S01 纯季包
    (release.episode == Some(episode_from) && release.episode_to == Some(episode_to))
        || (release.episode.is_none() && release.episode_to.is_none())
}

pub(crate) fn is_complete(subscribe: &Subscribe, facts: &SubscribeFacts) -> bool {
    if subscribe.wash_cut {
        return false;
    }
    match subscribe.coverage {
        Coverage::Movie => facts.movie().is_some(),
        Coverage::Tv {
            season,
            episode_from,
            episode_to,
        } => {
            let Some(to) = episode_to else {
                return false;
            };
            let capped = episode_from.saturating_add(Coverage::MAX_EPISODES.saturating_sub(1));
            (episode_from..=to.min(capped)).all(|ep| facts.get(Some(season), Some(ep)).is_some())
        }
    }
}

pub(crate) fn matches_media(
    media: &domain::Media,
    release: &Release,
    search_keywords: &[String],
) -> bool {
    if crate::media_identity::media_title_matches(media, &release.title) {
        return true;
    }
    // 候选关键词匹配（包含搜索生成器计算的各种衍生变体与年份组合）
    for kw in search_keywords {
        if crate::media_identity::title_matches(kw, &release.title) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests;
