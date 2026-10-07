use domain::{AtomRule, Confidence, Filter, Release, Torrent};

#[derive(Debug, Clone)]
pub struct ScoredTorrent {
    pub torrent: Torrent,
    pub release: Release,
    pub score: i32,
}

#[derive(Debug, Default)]
pub struct AdmitOutcome {
    pub admitted: Vec<ScoredTorrent>,
    pub rejected: Vec<ScoredTorrent>,
    pub skipped_low_confidence: Vec<Torrent>,
}

pub fn admit(torrents: Vec<Torrent>, filter: &Filter) -> AdmitOutcome {
    admit_scored(
        torrents
            .into_iter()
            .map(|torrent| (torrent, None))
            .collect(),
        filter,
    )
}

/// Admit torrents, optionally trusting a caller-corrected Release instead
/// of the parsed title. An override also bypasses the low-confidence skip.
pub fn admit_scored(torrents: Vec<(Torrent, Option<Release>)>, filter: &Filter) -> AdmitOutcome {
    let mut outcome = AdmitOutcome::default();
    for (torrent, override_release) in torrents {
        let (parsed, is_override) = match override_release {
            Some(release) => (release.clone(), true),
            None => (release::parse(&torrent.title), false),
        };
        if !is_override && parsed.confidence == Confidence::Low {
            tracing::debug!(torrent = %torrent.title, "低置信度种子跳过（无法可靠解析）");
            outcome.skipped_low_confidence.push(torrent);
            continue;
        }
        // 黑名单原子（exclude）命中任一 → 直接排除，不参与打分。
        let excluded = filter
            .atoms
            .iter()
            .filter(|atom| atom.exclude)
            .any(|atom| matches_atom(&torrent, &parsed, &atom.rule));
        let score = if excluded {
            0
        } else {
            score_one(&torrent, &parsed, filter)
        };
        let scored = ScoredTorrent {
            torrent,
            release: parsed,
            score,
        };
        if score > 0 {
            tracing::debug!(torrent = %scored.torrent.title, score, "候选种子通过过滤");
            outcome.admitted.push(scored);
        } else {
            tracing::debug!(torrent = %scored.torrent.title, excluded, "候选种子被过滤拒绝");
            outcome.rejected.push(scored);
        }
    }
    tracing::info!(
        admitted = outcome.admitted.len(),
        rejected = outcome.rejected.len(),
        skipped = outcome.skipped_low_confidence.len(),
        "过滤准入完成"
    );
    outcome
}

fn codec_matches(got: &str, want: &str) -> bool {
    let g = got.to_ascii_lowercase().replace('.', "");
    let w = want.to_ascii_lowercase().replace('.', "");
    if g == w {
        return true;
    }
    match w.as_str() {
        "hevc" | "h265" | "x265" => matches!(g.as_str(), "hevc" | "h265" | "x265"),
        "avc" | "h264" | "x264" => matches!(g.as_str(), "avc" | "h264" | "x264"),
        "av1" => g == "av1",
        _ => g.contains(&w) || w.contains(&g),
    }
}

fn score_one(torrent: &Torrent, parsed: &Release, filter: &Filter) -> i32 {
    // 检查是否存在正向 site 规则（站点优先级配置）
    let has_positive_site_rules = filter
        .atoms
        .iter()
        .any(|atom| !atom.exclude && matches!(atom.rule, AtomRule::Site(_)));

    // 检查是否存在其他正向规则（画质、来源、编码、体积、做种数等）
    let has_other_positive_rules = filter.atoms.iter().any(|atom| {
        !atom.exclude
            && !matches!(
                atom.rule,
                AtomRule::Site(_) | AtomRule::WashTarget(_) | AtomRule::UpgradeLadder(_)
            )
    });

    // 1. 如果既没有正向画质规则，也没有正向 site 规则（例如仅黑名单），保底准入 100 分
    if !has_positive_site_rules && !has_other_positive_rules {
        return 100;
    }

    // 2. 匹配正向规则
    let matched_atoms = filter
        .atoms
        .iter()
        .filter(|atom| !atom.exclude && matches_atom(torrent, parsed, &atom.rule))
        .collect::<Vec<_>>();

    // 3. 计算基础质量分 (Quality Score)
    // 如果没有配置任何其他正向规则，基础质量分默认保底 100（只要不被 exclude 排除）；
    // 如果配置了其他正向规则，则要求至少命中一项其他正向规则，得分取命中的最大 priority（保底 1）。
    let base_quality_score = if has_other_positive_rules {
        match matched_atoms
            .iter()
            .filter(|a| !matches!(a.rule, AtomRule::Site(_)))
            .map(|a| a.priority)
            .max()
        {
            Some(priority) => priority.max(1),
            None => return 0,
        }
    } else {
        100
    };

    // 4. 计算站点优先级增益 (Site Priority Tier)
    // 如果规则中配置了站点优先级：
    // - 命中的站点：增加站点阶梯分，保证高于未配置/未命中的站点。
    //   tier_bonus = matched_site_priority * 1000（保证阶梯优先级严格高于画质分，同时在同站点内由画质分决胜）
    // - 未命中指定站点的普通站点：保留匹配到的基础质量分作为回退候选。
    let site_bonus = matched_atoms
        .iter()
        .filter(|a| matches!(a.rule, AtomRule::Site(_)))
        .map(|a| a.priority)
        .max()
        .map_or(0, |p| p.max(1) * 1000);

    site_bonus + base_quality_score
}

fn matches_atom(torrent: &Torrent, parsed: &Release, rule: &AtomRule) -> bool {
    match rule {
        AtomRule::Resolution(want) => parsed
            .resolution
            .as_deref()
            .is_some_and(|got| got.eq_ignore_ascii_case(want)),
        AtomRule::Source(want) => parsed
            .source
            .as_deref()
            .is_some_and(|got| source_matches(got, want)),
        AtomRule::Free => torrent.free,
        AtomRule::Hr => torrent.hr,
        AtomRule::Codec(want) => parsed
            .codec
            .as_deref()
            .is_some_and(|got| codec_matches(got, want)),
        AtomRule::TitleMatch(want) => torrent
            .title
            .to_ascii_lowercase()
            .contains(&want.to_ascii_lowercase()),
        AtomRule::Hdr(want) => parsed
            .hdr
            .as_deref()
            .is_some_and(|got| got.eq_ignore_ascii_case(want)),
        AtomRule::Size { min_mb, max_mb } => {
            let Some(size_bytes) = torrent.size_bytes else {
                return false;
            };
            // 每集均摊：整季包用总体积 ÷ 集数。
            let episodes = u64::from(parsed.episode_count());
            let per_episode_mb = size_bytes / 1024 / 1024 / episodes.max(1);
            let (min, max) = match (min_mb, max_mb) {
                (Some(a), Some(b)) if a > b => (Some(*b), Some(*a)),
                (a, b) => (*a, *b),
            };
            min.map_or(true, |m| per_episode_mb >= m) && max.map_or(true, |m| per_episode_mb <= m)
        }
        AtomRule::MinSeeders(n) => torrent.seeders.map_or(false, |seeders| seeders >= *n),
        AtomRule::SubtitleLanguage(want) => parsed
            .subtitle_language
            .as_deref()
            .is_some_and(|got| lang_matches(got, want)),
        AtomRule::AudioLanguage(want) => parsed
            .audio_language
            .as_deref()
            .is_some_and(|got| lang_matches(got, want)),
        AtomRule::Site(want) => torrent.site_id.to_string().eq_ignore_ascii_case(want),
        AtomRule::WashTarget(_) => false,    // 洗版目标不参与 admit
        AtomRule::UpgradeLadder(_) => false, // 洗版比较维度不参与 admit
    }
}

/// 语言匹配：完全一致，或 `zh` 命中任意中文字幕/音轨（简/繁）。
fn lang_matches(got: &str, want: &str) -> bool {
    let got = got.to_ascii_lowercase();
    let want = want.to_ascii_lowercase();
    got == want || (want == "zh" && got.starts_with("zh"))
}

/// 片源匹配：忽略连字符和下划线，支持族匹配（rip 包含 webrip/dvdrip，tv 包含 hdtv）
fn source_matches(got: &str, want: &str) -> bool {
    let g = got.to_ascii_lowercase().replace('-', "").replace('_', "");
    let w = want.to_ascii_lowercase().replace('-', "").replace('_', "");
    if g == w {
        return true;
    }
    match w.as_str() {
        "bluray" => g == "bluray",
        "webdl" => g == "webdl",
        "rip" => g.contains("rip"),
        "tv" => g == "hdtv" || g == "tv",
        _ => false,
    }
}
