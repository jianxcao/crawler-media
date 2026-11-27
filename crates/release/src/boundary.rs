type SeasonEpisodeBoundary = (usize, Option<u32>, Option<u32>, Option<u32>);

/// 剥离段首的发布标签，如 [中字] 或 【肥猫发布】
pub(super) fn strip_leading_brackets(s: &str) -> &str {
    let mut cur = s.trim();
    while (cur.starts_with('[') && cur.contains(']'))
        || (cur.starts_with('【') && cur.contains('】'))
    {
        if cur.starts_with('[') {
            if let Some(end) = cur.find(']') {
                cur = cur[end + ']'.len_utf8()..].trim();
            } else {
                break;
            }
        } else if cur.starts_with('【') {
            if let Some(end) = cur.find('】') {
                cur = cur[end + '】'.len_utf8()..].trim();
            } else {
                break;
            }
        } else {
            break;
        }
    }
    cur
}

/// 借鉴 MoviePilot：从完整原始串中定位季集边界标记，并精准切分出前面的干净片名
pub(super) fn extract_boundary_season_episode(
    s: &str,
) -> (Option<String>, Option<u32>, Option<u32>, Option<u32>) {
    let upper = s.to_ascii_uppercase();

    // 格式 1: S01E05、S01 E01-E04、S01-S02、S01 (单独季)
    if let Some((pos, season, ep, ep_to)) = find_s_e_pattern(&upper) {
        let title = normalized_boundary_title(&s[..pos]);
        if !title.is_empty() && (season.is_some() || ep.is_some()) {
            return (Some(title), season, ep, ep_to);
        }
    }

    // 格式 2: 中文 "第一季" / "第 2 季" / "第 5 集" / "第01-04集"
    if let Some((pos, season, ep, ep_to)) = find_chinese_season_episode(s) {
        let title = normalized_boundary_title(&s[..pos]);
        if !title.is_empty() {
            return (Some(title), season, ep, ep_to);
        }
    }

    (None, None, None, None)
}

fn normalized_boundary_title(raw: &str) -> String {
    let mut words: Vec<&str> = raw
        .split(['.', '_', ' ', '[', ']', '(', ')'])
        .filter(|word| !word.is_empty() && *word != "-")
        .collect();
    if words
        .last()
        .is_some_and(|word| crate::attributes::parse_year(word).is_some())
    {
        words.pop();
    }
    words.join(" ")
}

/// 集号只接受 1 到 999。8 位 `20YYMMDD` 是日期，即使前面写了 `E` 也不是集。
pub(super) fn plausible_episode(digits: &str) -> bool {
    let Ok(episode) = digits.parse::<u32>() else {
        return false;
    };
    if !(1..=999).contains(&episode) {
        return false;
    }
    if digits.len() == 8 && is_calendar_date(digits) {
        return false;
    }
    true
}

pub(super) fn reject_date_token(token: &str) {
    tracing::warn!(token, "日期格式被拒绝为集号");
}

pub(super) fn is_calendar_date_text(digits: &str) -> bool {
    is_calendar_date(digits)
}

fn is_calendar_date(digits: &str) -> bool {
    if digits.len() != 8 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let year = &digits[..4];
    let month: u32 = digits[4..6].parse().unwrap_or(0);
    let day: u32 = digits[6..8].parse().unwrap_or(0);
    crate::attributes::parse_year(year).is_some()
        && (1..=12).contains(&month)
        && (1..=31).contains(&day)
}

fn is_likely_resolution(bytes: &[u8], ep: u32, m: usize) -> bool {
    matches!(ep, 480 | 576 | 720 | 1080 | 2160 | 4320)
        || (m < bytes.len() && matches!(bytes[m], b'P' | b'p' | b'I' | b'i'))
}

fn try_parse_s_followed(
    upper: &str,
    bytes: &[u8],
    len: usize,
    i: usize,
) -> Option<SeasonEpisodeBoundary> {
    let mut j = i + 1;
    while j < len && bytes[j].is_ascii_digit() {
        j += 1;
    }
    let season = upper[i + 1..j].parse::<u32>().ok()?;
    if season >= 100 {
        return None;
    }
    let mut k = j;
    while k < len && matches!(bytes[k], b' ' | b'.' | b'_' | b'-') {
        k += 1;
    }
    if k < len && (bytes[k] == b'E' || (bytes[k] == b'E' && k + 1 < len && bytes[k + 1] == b'P')) {
        let e_start = if k + 1 < len && bytes[k] == b'E' && bytes[k + 1] == b'P' {
            k + 2
        } else {
            k + 1
        };
        let mut m = e_start;
        while m < len && bytes[m].is_ascii_digit() {
            m += 1;
        }
        // Explicit `S00E00` is a real special coordinate; elsewhere episode 0 stays implausible.
        let special_zero = season == 0 && upper[e_start..m].parse::<u32>().ok() == Some(0);
        if m > e_start
            && is_episode_digit_end(bytes, m)
            && (plausible_episode(&upper[e_start..m]) || special_zero)
        {
            let ep = upper[e_start..m].parse::<u32>().ok()?;
            let after_ep = skip_ep_version_suffix(bytes, m);
            let Ok(ep_to) = parse_episode_range(ep, &upper[after_ep..]) else {
                return None;
            };
            return Some((i, Some(season), Some(ep), ep_to));
        }
    }
    if k < len && bytes[k].is_ascii_digit() {
        let mut m = k;
        while m < len && bytes[m].is_ascii_digit() {
            m += 1;
        }
        if is_episode_digit_end(bytes, m)
            && plausible_episode(&upper[k..m])
            && let Ok(ep) = upper[k..m].parse::<u32>()
            && !is_likely_resolution(bytes, ep, m)
        {
            let after_ep = skip_ep_version_suffix(bytes, m);
            let Ok(ep_to) = parse_episode_range(ep, &upper[after_ep..]) else {
                return None;
            };
            return Some((i, Some(season), Some(ep), ep_to));
        }
    }
    if j == len || !bytes[j].is_ascii_alphanumeric() {
        return Some((i, Some(season), None, None));
    }
    None
}

fn skip_ep_version_suffix_raw(bytes: &[u8], j: usize) -> usize {
    if bytes.get(j) == Some(&b'V') && bytes.get(j + 1).is_some_and(u8::is_ascii_digit) {
        let mut k = j + 1;
        while k < bytes.len() && bytes[k].is_ascii_digit() {
            k += 1;
        }
        return k;
    }
    j
}

pub(super) const GLUED_CODECS: &[&str] = &[
    "X265", "X264", "H.265", "H.264", "H265", "H264", "HEVC", "AVC", "AV1", "MPEG2", "VC-1", "VC1",
    "XVID", "DIVX",
];

fn has_glued_codec_tail(bytes: &[u8], k: usize) -> bool {
    for pat_str in GLUED_CODECS {
        let pat = pat_str.as_bytes();
        if bytes.get(k..k + pat.len()) == Some(pat) {
            let end = k + pat.len();
            return end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        }
    }
    false
}

const MAX_CHAINED_EP_SEGMENTS: usize = domain::Coverage::MAX_EPISODES as usize;

fn has_chained_ep_tail(bytes: &[u8], mut k: usize) -> bool {
    let mut segments = 0usize;
    loop {
        if segments >= MAX_CHAINED_EP_SEGMENTS || bytes.get(k) != Some(&b'E') {
            return false;
        }
        segments += 1;
        k += 1;
        if bytes.get(k) == Some(&b'P') {
            k += 1;
        }
        let start = k;
        while k < bytes.len() && bytes[k].is_ascii_digit() {
            k += 1;
        }
        let dlen = k - start;
        if dlen == 0 || dlen > 4 {
            return false;
        }
        let Ok(digits) = std::str::from_utf8(&bytes[start..k]) else {
            return false;
        };
        if !plausible_episode(digits) {
            return false;
        }
        let next = skip_ep_version_suffix_raw(bytes, k);
        if next >= bytes.len()
            || !bytes[next].is_ascii_alphanumeric()
            || has_glued_codec_tail(bytes, next)
        {
            return true;
        }
        k = next;
    }
}

fn skip_ep_version_suffix(bytes: &[u8], j: usize) -> usize {
    let k = skip_ep_version_suffix_raw(bytes, j);
    if k > j
        && (k == bytes.len()
            || !bytes[k].is_ascii_alphanumeric()
            || has_glued_codec_tail(bytes, k)
            || has_chained_ep_tail(bytes, k))
    {
        return k;
    }
    j
}

pub(super) fn is_episode_digit_end(bytes: &[u8], j: usize) -> bool {
    let k = skip_ep_version_suffix_raw(bytes, j);
    k >= bytes.len()
        || !bytes[k].is_ascii_alphanumeric()
        || has_glued_codec_tail(bytes, k)
        || has_chained_ep_tail(bytes, k)
}

/// 查找 SxxExx、Sxx Exx、Sxx、Exx
pub(super) fn find_s_e_pattern(upper: &str) -> Option<SeasonEpisodeBoundary> {
    let bytes = upper.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        let is_start = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if is_start
            && bytes[i] == b'S'
            && i + 1 < len
            && bytes[i + 1].is_ascii_digit()
            && let Some(res) = try_parse_s_followed(upper, bytes, len, i)
        {
            return Some(res);
        }

        // 单独以 E01 / EP01 开头
        let standalone_e_start = if is_start && bytes[i] == b'E' {
            if bytes.get(i + 1) == Some(&b'P') && bytes.get(i + 2).is_some_and(u8::is_ascii_digit) {
                Some(i + 2)
            } else if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
                Some(i + 1)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(e_start) = standalone_e_start {
            let mut j = e_start;
            while j < len && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if is_episode_digit_end(bytes, j) {
                let digits = &upper[e_start..j];
                if !plausible_episode(digits) {
                    if is_calendar_date(digits) {
                        reject_date_token(&upper[i..j]);
                    }
                } else if let Ok(ep) = digits.parse::<u32>() {
                    let after_ep = skip_ep_version_suffix(bytes, j);
                    if let Ok(ep_to) = parse_episode_range(ep, &upper[after_ep..]) {
                        let ep_to = ep_to.filter(|to| plausible_episode(&to.to_string()));
                        return Some((i, Some(1), Some(ep), ep_to));
                    }
                }
            }
        }

        i += 1;
    }

    None
}

/// 解析 E01-E04、-04 或紧邻多集 E01E02E03 这种区间
fn parse_episode_range(prev_ep: u32, after: &str) -> Result<Option<u32>, ()> {
    let bytes = after.as_bytes();
    if bytes.first().is_some_and(|b| *b == b'E' || *b == b'e') {
        let mut cur = 0;
        let mut cur_min = prev_ep;
        let mut last_to = None;
        while cur < bytes.len() && (bytes[cur] == b'E' || bytes[cur] == b'e') {
            let mut k = cur + 1;
            if bytes.get(k).is_some_and(|b| *b == b'P' || *b == b'p') {
                k += 1;
            }
            let dstart = k;
            while k < bytes.len() && bytes[k].is_ascii_digit() {
                k += 1;
            }
            if k == dstart || !is_episode_digit_end(bytes, k) {
                return Err(());
            }
            let digits = &after[dstart..k];
            if !plausible_episode(digits) {
                return Err(());
            }
            let Ok(ep) = digits.parse::<u32>() else {
                return Err(());
            };
            if ep < cur_min {
                return Err(());
            }
            cur_min = ep;
            last_to = Some(ep);
            cur = skip_ep_version_suffix(bytes, k);
        }
        return Ok(last_to);
    }
    let mut i = 0;
    let mut has_sep = false;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'-' || bytes[i] == b'~') {
        if bytes[i] == b'-' || bytes[i] == b'~' {
            has_sep = true;
        }
        i += 1;
    }
    if !has_sep || i >= bytes.len() {
        return Ok(None);
    }
    let mut k = i;
    if bytes[k] == b'E' || bytes[k] == b'e' {
        k += 1;
        if k < bytes.len() && (bytes[k] == b'P' || bytes[k] == b'p') {
            k += 1;
        }
    }
    let digit_start = k;
    while k < bytes.len() && bytes[k].is_ascii_digit() {
        k += 1;
    }
    // Digits running into non-version letters are an attribute, not a range end:
    // `S01E90 - 2160p`, `E05-720p`, `- 1080i`, `- 4K`.
    if !is_episode_digit_end(bytes, k) {
        return Ok(None);
    }
    if k > digit_start
        && !is_calendar_date(&after[digit_start..k])
        && let Ok(to) = after[digit_start..k].parse::<u32>()
        && to > 0
        && to >= prev_ep
    {
        return Ok(Some(to));
    }
    Ok(None)
}

/// 解析中文季集如 "第 2 季 第 5 集"、"第二季 第01-04集"
pub(super) fn find_chinese_season_episode(s: &str) -> Option<SeasonEpisodeBoundary> {
    let mut pos = None;
    let mut season = None;
    let mut episode = None;
    let mut episode_to = None;

    // 遍历所有 "第" 出现的位置
    for (p, _) in s.match_indices('第') {
        let after = &s[p + '第'.len_utf8()..];

        // 检查是否是第x季
        if let Some(j_pos) = after.find('季')
            && j_pos <= 10
        {
            let season_str = after[..j_pos].trim();
            if let Some(s_num) = parse_cn_or_arabic_num(season_str) {
                if pos.is_none_or(|cur| p < cur) {
                    pos = Some(p);
                }
                season = Some(s_num);
            }
        }

        // 检查是否是第x集 / 话
        if let Some(end_pos) = after
            .find('集')
            .or_else(|| after.find('话'))
            .or_else(|| after.find('話'))
            && end_pos <= 20
        {
            let ep_str = after[..end_pos].trim();
            let (from, to) = parse_chinese_ep_range(ep_str);
            if from.is_some() {
                if pos.is_none_or(|cur| p < cur) {
                    pos = Some(p);
                }
                episode = from;
                episode_to = to;
            }
        }
    }

    if episode.is_some() && season.is_none() {
        season = Some(1);
    }

    pos.map(|p| (p, season, episode, episode_to))
}

fn parse_chinese_ep_range(s: &str) -> (Option<u32>, Option<u32>) {
    let parts: Vec<&str> = s.split(['-', '~', '—']).collect();
    match parts.as_slice() {
        [single] => (parse_cn_or_arabic_num(single.trim()), None),
        [from, to] => {
            let f = parse_cn_or_arabic_num(from.trim());
            let t = parse_cn_or_arabic_num(to.trim());
            (
                f,
                t.filter(|to_val| f.is_some_and(|f_val| *to_val >= f_val)),
            )
        }
        _ => (None, None),
    }
}

fn parse_cn_or_arabic_num(s: &str) -> Option<u32> {
    if let Ok(n) = s.parse::<u32>() {
        return Some(n);
    }
    match s {
        "一" | "1" => Some(1),
        "二" | "两" | "2" => Some(2),
        "三" | "3" => Some(3),
        "四" | "4" => Some(4),
        "五" | "5" => Some(5),
        "六" | "6" => Some(6),
        "七" | "7" => Some(7),
        "八" | "8" => Some(8),
        "九" | "9" => Some(9),
        "十" | "10" => Some(10),
        _ => None,
    }
}
