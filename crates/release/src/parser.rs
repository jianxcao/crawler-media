//! Release parsing from names and unit boundaries.
use domain::{Confidence, Release};

use crate::attributes::*;
use crate::boundary;
use crate::boundary::*;

pub fn parse(raw: &str) -> Release {
    // 分词（优先从原始标题提取语言等属性，避免段首 [中字] 等标签被提前剥离导致丢失）
    let raw_tokens = tokenize(raw);
    let subtitle_language = raw_tokens.iter().find_map(|t| parse_subtitle_language(t));
    let audio_language = raw_tokens.iter().find_map(|t| parse_audio_language(t));

    // 0. 借鉴 MovieClaw / MoviePilot：剥离段首发布标签，如 [中字]、[肥猫发布] 等
    let clean_raw = strip_leading_brackets(raw);

    // 1. 优先使用基于整串上下文的季集和标题切分（处理 S01E01、S01 E01-E04、S01、Season 1、第x季第x集等）
    let (boundary_title, boundary_season, boundary_ep, boundary_ep_to) =
        extract_boundary_season_episode(clean_raw);

    // 分词时注意保留像 H.264 中的点，但切分常见的间隔符
    // 遇到包含字母和点混杂（例如 H.264）的不应该盲目按点拆散
    let tokens = tokenize(clean_raw);

    let year = tokens.iter().find_map(|t| parse_year(t));
    let resolution = tokens.iter().find_map(|t| parse_resolution(t));
    // 遇到包含 REMUX 和 BluRay 组合的情况，REMUX 拥有更高片源优先级
    let source = if tokens.iter().any(|t| t.eq_ignore_ascii_case("remux")) {
        Some("Remux".to_string())
    } else {
        tokens.iter().find_map(|t| parse_source(t))
    };
    let codec = tokens.iter().find_map(|t| parse_codec(t));
    let hdr = tokens.iter().find_map(|t| parse_hdr(t));

    // 2. 如果 boundary 没匹配出季或集，进行多 token 流状态机扫描（对齐 MoviePilot MetaVideo）
    let (season, episode, episode_to) = if boundary_season.is_some() || boundary_ep.is_some() {
        (boundary_season, boundary_ep, boundary_ep_to)
    } else {
        scan_tokens_for_season_episode(&tokens)
    };

    let group = raw.rsplit_once('-').map(|(_, g)| g.trim().to_string());

    let title = if let Some(bt) = boundary_title {
        bt
    } else {
        let mut title_tokens = Vec::new();
        for token in &tokens {
            if token == "-" {
                continue;
            }
            if parse_year(token).is_some()
                || parse_resolution(token).is_some()
                || parse_source(token).is_some()
                || parse_codec(token).is_some()
                || parse_hdr(token).is_some()
                || is_season_or_episode_token(token)
            {
                break;
            }
            title_tokens.push(token.as_str());
        }
        if title_tokens.is_empty() {
            raw.to_string()
        } else {
            title_tokens.join(" ")
        }
    };

    let confidence = if year.is_some()
        || resolution.is_some()
        || source.is_some()
        || season.is_some()
        || episode.is_some()
    {
        Confidence::High
    } else {
        Confidence::Low
    };

    let release = Release {
        title,
        year,
        season,
        episode,
        episode_to,
        resolution,
        source,
        codec,
        hdr,
        subtitle_language,
        audio_language,
        group,
        confidence,
        season_inferred: season.is_some() && !has_explicit_season(clean_raw),
    };
    tracing::debug!(
        raw = %raw,
        title = %release.title,
        season = ?release.season,
        episode = ?release.episode,
        resolution = ?release.resolution,
        source = ?release.source,
        codec = ?release.codec,
        confidence = ?release.confidence,
        "解析发布名"
    );
    release
}

/// True when `raw` names a season itself (`S01`, `S1E02`, `Season 2`, `第二季`).
pub fn has_explicit_season(raw: &str) -> bool {
    if raw.contains('季') {
        return true;
    }
    let upper = raw.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    (0..bytes.len()).any(|i| {
        let at_word = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        at_word
            && bytes[i] == b'S'
            && (bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
                || upper[i..].starts_with("SEASON"))
    })
}

fn tokenize(raw: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = raw.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let c = chars[i];
        if c == ' '
            || c == '_'
            || c == '['
            || c == ']'
            || c == '('
            || c == ')'
            || c == '【'
            || c == '】'
        {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
        } else if c == '.' {
            // 如果是 H.264 这种特定格式，保留点（含紧贴集号如 S01E01H.264）
            let ends_glued_h = cur.len() >= 2
                && matches!(cur.as_bytes()[cur.len() - 1], b'H' | b'h')
                && cur.as_bytes()[cur.len() - 2].is_ascii_digit();
            let is_h264 = (cur.eq_ignore_ascii_case("H") || ends_glued_h)
                && i + 3 < len
                && chars[i + 1] == '2'
                && chars[i + 2] == '6'
                && (chars[i + 3] == '4' || chars[i + 3] == '5')
                && (i + 4 >= len || !chars[i + 4].is_ascii_alphanumeric());
            if is_h264 {
                cur.push(c);
            } else if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
        i += 1;
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

fn strip_ep_token_digits_prefix(upper: &str) -> Option<&str> {
    let rest = upper
        .strip_prefix("EP")
        .or_else(|| upper.strip_prefix('E'))?;
    let bytes = rest.as_bytes();
    let mut j = 0;
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        j += 1;
    }
    if j == 0 || !boundary::is_episode_digit_end(bytes, j) {
        return None;
    }
    Some(upper)
}

fn is_season_or_episode_token(token: &str) -> bool {
    let upper = token.to_ascii_uppercase();
    if upper.starts_with('S') && upper[1..].chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if upper.starts_with('S')
        && let Some(e_pos) = upper.find('E')
        && e_pos > 1
        && upper[1..e_pos].bytes().all(|b| b.is_ascii_digit())
        && parse_ep_numbers(&upper[e_pos..]).0.is_some()
    {
        return true;
    }
    if strip_ep_token_digits_prefix(&upper).is_some() {
        return true;
    }
    if upper == "SEASON" || upper == "EPISODE" {
        return true;
    }
    false
}

/// 模拟 MoviePilot 的 token 流扫描：支持独立 S01 + E01-E04、Season 1、E01 等
fn scan_tokens_for_season_episode(tokens: &[String]) -> (Option<u32>, Option<u32>, Option<u32>) {
    let mut season: Option<u32> = None;
    let mut episode: Option<u32> = None;
    let mut episode_to: Option<u32> = None;
    let mut last_was_season_word = false;
    let mut last_was_episode_word = false;
    let mut just_saw_season = false;

    for token in tokens {
        let prev_was_season = just_saw_season;
        just_saw_season = false;
        let upper = token.to_ascii_uppercase();

        // 1. Season xx
        if last_was_season_word {
            last_was_season_word = false;
            if let Ok(s) = token.parse::<u32>()
                && season.is_none()
                && s < 100
            {
                season = Some(s);
                just_saw_season = true;
                continue;
            }
        }
        if upper == "SEASON" {
            last_was_season_word = true;
            continue;
        }

        // 2. Episode xx / Episode 01v2 / Episode 01-02
        if last_was_episode_word {
            last_was_episode_word = false;
            let (from, to) = parse_ep_numbers(&upper);
            if let Some(e) = from
                && episode.is_none()
            {
                episode = Some(e);
                episode_to = to;
                if season.is_none() {
                    season = Some(1);
                }
                continue;
            }
        }
        if upper == "EPISODE" {
            last_was_episode_word = true;
            continue;
        }

        // 3. S01E01 或 S01E01-E04 或 S01
        if let Some(after_season) = upper.strip_prefix('S')
            && let Some((s, ep, ep_to)) = parse_s_prefixed_token(&upper, after_season)
        {
            if season.is_none() {
                season = Some(s);
            }
            if ep.is_some() {
                episode = ep;
                episode_to = ep_to;
                break;
            }
            just_saw_season = true;
            continue;
        }

        // 4. E01-E04 或 EP01
        if let Some(ep_part) = strip_ep_token_digits_prefix(&upper) {
            let (from, to) = parse_ep_numbers(ep_part);
            if from.is_some() {
                if episode.is_none() {
                    episode = from;
                    episode_to = to;
                    if season.is_none() {
                        season = Some(1);
                    }
                }
                break;
            }
        }

        // 5. Bare numeric / versioned episode or range right after explicit season (`S01 01v2-02v2`)
        if prev_was_season
            && season.is_some()
            && episode.is_none()
            && upper.as_bytes().first().is_some_and(u8::is_ascii_digit)
            && parse_resolution(token).is_none()
            && parse_year(token).is_none()
        {
            let (from, to) = parse_ep_numbers(&upper);
            if from.is_some() {
                episode = from;
                episode_to = to;
                break;
            }
        }
    }

    (season, episode, episode_to)
}

fn parse_s_prefixed_token(
    upper: &str,
    after_season: &str,
) -> Option<(u32, Option<u32>, Option<u32>)> {
    if let Some(e_pos) = upper.find('E') {
        if e_pos <= 1 {
            return None;
        }
        let s = upper[1..e_pos].parse::<u32>().ok().filter(|&s| s < 100)?;
        let ep_part = &upper[e_pos..];
        let stripped_ep = strip_trailing_glued_codec(ep_part);
        if s == 0
            && matches!(
                strip_ep_version_suffix(stripped_ep),
                "E0" | "E00" | "EP0" | "EP00"
            )
        {
            return Some((0, Some(0), None));
        }
        let (from, to) = parse_ep_numbers(ep_part);
        return from.map(|ep| (s, Some(ep), to));
    }
    if !after_season.is_empty()
        && after_season.bytes().all(|b| b.is_ascii_digit())
        && let Ok(s) = after_season.parse::<u32>()
        && s < 100
    {
        return Some((s, None, None));
    }
    None
}

fn strip_ep_version_suffix(seg: &str) -> &str {
    let trimmed = seg
        .strip_prefix("EP")
        .or_else(|| seg.strip_prefix('E'))
        .unwrap_or(seg);
    let bytes = trimmed.as_bytes();
    let mut j = 0;
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        j += 1;
    }
    if j > 0
        && j + 1 < bytes.len()
        && bytes[j] == b'V'
        && bytes[j + 1..].iter().all(u8::is_ascii_digit)
    {
        let prefix_len = seg.len() - trimmed.len();
        return &seg[..prefix_len + j];
    }
    seg
}

fn skip_leading_v_digits(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.first() == Some(&b'V') && bytes.get(1).is_some_and(u8::is_ascii_digit) {
        let mut k = 1;
        while k < bytes.len() && bytes[k].is_ascii_digit() {
            k += 1;
        }
        return &s[k..];
    }
    s
}

fn strip_trailing_glued_codec(s: &str) -> &str {
    for codec in boundary::GLUED_CODECS {
        if let Some(prefix) = s.strip_suffix(codec)
            && prefix.as_bytes().last().is_some_and(u8::is_ascii_digit)
        {
            return prefix;
        }
    }
    s
}

fn extract_seg_episodes<'a>(seg: &'a str, out: &mut Vec<&'a str>) {
    let seg = strip_trailing_glued_codec(seg);
    let s = strip_ep_version_suffix(seg);
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        out.push(s);
        return;
    }
    let mark = out.len();
    let mut cur = seg;
    let mut count = 0usize;
    loop {
        if count >= domain::Coverage::MAX_EPISODES as usize {
            out.truncate(mark);
            out.push("0");
            return;
        }
        let Some(rest) = cur.strip_prefix("EP").or_else(|| cur.strip_prefix('E')) else {
            out.truncate(mark);
            return;
        };
        let dlen = rest.bytes().take_while(u8::is_ascii_digit).count();
        if dlen == 0 {
            out.truncate(mark);
            return;
        }
        out.push(&rest[..dlen]);
        count += 1;
        let tail = skip_leading_v_digits(&rest[dlen..]);
        if tail.is_empty() {
            return;
        }
        if tail.starts_with('E') {
            cur = tail;
        } else {
            out.truncate(mark);
            out.push("0");
            return;
        }
    }
}

fn parse_ep_numbers(ep_part: &str) -> (Option<u32>, Option<u32>) {
    let cleaned = strip_trailing_glued_codec(ep_part);
    let segs: Vec<&str> = cleaned
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    let mut raw_digits = Vec::new();
    for seg in &segs {
        extract_seg_episodes(seg, &mut raw_digits);
    }
    if let Some(token) = raw_digits
        .iter()
        .copied()
        .find(|text| text.len() == 8 && boundary::is_calendar_date_text(text))
    {
        boundary::reject_date_token(token);
        return (None, None);
    }
    if raw_digits.is_empty() || !raw_digits.iter().all(|text| plausible_episode(text)) {
        return (None, None);
    }
    let mut parsed = Vec::with_capacity(raw_digits.len());
    for text in raw_digits {
        let Ok(v) = text.parse::<u32>() else {
            return (None, None);
        };
        parsed.push(v);
    }
    match parsed.as_slice() {
        [from] => (Some(*from), None),
        [from, rest @ ..] if parsed.windows(2).all(|w| w[1] >= w[0]) => {
            (Some(*from), rest.last().copied())
        }
        _ => (None, None),
    }
}

#[cfg(test)]
mod season_inferred_tests {
    #[test]
    fn missing_season_marker_is_flagged_as_inferred() {
        let r = super::parse("Show Episode 13 1080p WEB-DL");
        assert_eq!(
            (r.season, r.episode, r.season_inferred),
            (Some(1), Some(13), true)
        );
        assert!(super::parse("Show E05 1080p").season_inferred);
        assert!(!super::parse("Show.S01E05.1080p").season_inferred);
        assert!(!super::parse("Show Season 2 Episode 3").season_inferred);
        assert!(!super::parse("剧名 第二季 第03集").season_inferred);
        assert!(!super::parse("Movie.2020.1080p").season_inferred);
    }
}
