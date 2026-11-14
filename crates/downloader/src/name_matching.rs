//! Display name matching and exact size checks.
/// 归一化标题（保留字母数字及中文字符、小写），用于跨客户端匹配任务。
pub fn normalize_name(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

/// 标题宽松匹配（双向包含，或分词显著重合）。
///
/// # Warning (D-L3)
/// This is a loose, display-only heuristic: unrelated tasks with similar
/// titles match. Never use it to choose a task to remove, pause, claim or
/// delete files of; destructive paths must use ownership tags / info hash.
/// `api/tests/subscription_e2e/d_l3_loose_match_audit.rs` audits call sites.
pub fn names_match(client_name: &str, want: &str) -> bool {
    let got = normalize_name(client_name);
    let want_norm = normalize_name(want);
    if !want_norm.is_empty()
        && !got.is_empty()
        && (got == want_norm || got.contains(&want_norm) || want_norm.contains(&got))
    {
        return true;
    }

    // 分词重合判断：应对 PT 站标题与 torrent 文件名中英文前后缀变体
    // （例如 PT 发帖名：Once upon a Time in Longfan S01 E01-E04 ...
    //   客户端内任务名：法医秦明之龙番往事.Once.upon.a.Time.in.Longfan.S01 ...）
    let words_got = extract_name_tokens(client_name);
    let words_want = extract_name_tokens(want);
    if !words_got.is_empty() && !words_want.is_empty() {
        let common = words_got.intersection(&words_want).count();
        let min_len = words_got.len().min(words_want.len());
        if min_len >= 2 && common * 2 >= min_len {
            return true;
        }
    }

    false
}

pub(crate) fn extract_name_tokens(s: &str) -> std::collections::HashSet<String> {
    let mut tokens = std::collections::HashSet::new();
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            cur.extend(c.to_lowercase());
        } else if !cur.is_empty() {
            let tok = std::mem::take(&mut cur);
            if !is_ignored_token(&tok) {
                tokens.insert(tok);
            }
        }
    }
    if !cur.is_empty() && !is_ignored_token(&cur) {
        tokens.insert(cur);
    }
    tokens
}

fn is_ignored_token(t: &str) -> bool {
    matches!(
        t,
        "2160p"
            | "1080p"
            | "720p"
            | "480p"
            | "4k"
            | "uhd"
            | "web"
            | "dl"
            | "webdl"
            | "webrip"
            | "hdtv"
            | "bluray"
            | "h264"
            | "h265"
            | "x264"
            | "x265"
            | "hevc"
            | "avc"
            | "aac"
            | "ddp"
            | "dts"
            | "flac"
    ) || (t.starts_with('e') && t[1..].chars().all(|c| c.is_ascii_digit()))
}

/// 体积精确匹配（可选）。
pub(crate) fn size_matches(got: u64, want: Option<u64>) -> bool {
    want.is_some_and(|want| want > 0 && got == want)
}

#[cfg(test)]
mod match_tests {
    use crate::torrent_matches_snapshot;

    #[test]
    fn same_size_different_title_does_not_match() {
        assert!(!torrent_matches_snapshot(
            "The Matrix 1999 1080p",
            Some(1_000_000),
            "Once Upon a Time 2024 1080p",
            1_000_000,
        ));
    }

    #[test]
    fn matching_title_and_size_matches() {
        assert!(torrent_matches_snapshot(
            "Movie.A.2024.1080p",
            Some(1_000_000),
            "Movie.A.2024.1080p",
            1_000_000,
        ));
    }

    #[test]
    fn unknown_size_still_matches_title() {
        assert!(torrent_matches_snapshot(
            "Movie.A.2024.1080p",
            None,
            "Movie.A.2024.1080p",
            1_000_000,
        ));
    }
}
