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
    // A blacklist-only group admits every surviving candidate. Wash policy
    // atoms describe comparisons, not positive admission requirements.
    if !filter.atoms.iter().any(|atom| {
        !atom.exclude
            && !matches!(
                atom.rule,
                AtomRule::WashTarget(_) | AtomRule::UpgradeLadder(_)
            )
    }) {
        return 100;
    }
    let matched_atoms = filter
        .atoms
        .iter()
        .filter(|atom| !atom.exclude && matches_atom(torrent, parsed, &atom.rule))
        .collect::<Vec<_>>();

    if matched_atoms.is_empty() {
        return 0;
    }

    // 只要有任一非排除规则命中，基础保底分至少为 1（若配置 priority 均为 0 也算准入）
    let max_p = matched_atoms.iter().map(|a| a.priority).max().unwrap_or(0);
    max_p.max(1)
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
