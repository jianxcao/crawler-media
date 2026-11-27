use scraper::{Html, Selector};

use crate::IndexerError;

/// Parse NexusPHP user traffic statistics `(uploaded_bytes, downloaded_bytes)` from `index.php` HTML.
pub(crate) fn parse_nexusphp_user_stats(html: &str) -> Result<Option<(u64, u64)>, IndexerError> {
    let doc = Html::parse_document(html);
    if looks_like_login_page(&doc, html) {
        return Err(IndexerError::Parse(
            "站点 Cookie 已过期或失效，无法读取用户统计".into(),
        ));
    }

    let container_text = extract_stats_container_text(&doc)
        .unwrap_or_else(|| doc.root_element().text().collect::<Vec<_>>().join(" "));
    let compact = normalize_whitespace(&container_text);

    let uploaded = find_metric_bytes(&compact, &["上传量", "上传", "Uploaded", "Up:", "上行"]);
    let downloaded =
        find_metric_bytes(&compact, &["下载量", "下载", "Downloaded", "Down:", "下行"]);

    match (uploaded, downloaded) {
        (Some(up), Some(down)) => Ok(Some((up, down))),
        (Some(up), None) if compact.contains("下载") || compact.contains("Downloaded") => {
            Ok(Some((up, 0)))
        }
        _ => Ok(None),
    }
}

fn looks_like_login_page(doc: &Html, body: &str) -> bool {
    if body.contains("logout.php") || body.contains("data-uploader-actions") {
        return false;
    }
    if let Ok(sel) = Selector::parse("form[action*='login'], form[action*='takelogin']")
        && doc.select(&sel).next().is_some()
    {
        return true;
    }
    false
}

fn extract_stats_container_text(doc: &Html) -> Option<String> {
    for selector_str in ["#info_block", ".user-info", ".site-userbar", "#outer"] {
        if let Ok(sel) = Selector::parse(selector_str)
            && let Some(el) = doc.select(&sel).next()
        {
            let text = el.text().collect::<Vec<_>>().join(" ");
            if text.contains("上传")
                || text.contains("Uploaded")
                || text.contains("下载")
                || text.contains("Downloaded")
            {
                return Some(text);
            }
        }
    }
    None
}

fn normalize_whitespace(input: &str) -> String {
    input
        .replace('\u{a0}', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn find_metric_bytes(text: &str, labels: &[&str]) -> Option<u64> {
    for label in labels {
        let mut search_from = 0usize;
        while let Some(pos) = find_case_insensitive(&text[search_from..], label) {
            let after_idx = search_from + pos + label.len();
            let tail = text[after_idx..]
                .trim_start_matches([' ', ':', '：', '\t', '[', ']'])
                .trim_start();
            if let Some(bytes) = parse_size_prefix(tail) {
                return Some(bytes);
            }
            search_from = after_idx;
        }
    }
    None
}

fn find_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_ascii() {
        haystack
            .to_ascii_lowercase()
            .find(&needle.to_ascii_lowercase())
    } else {
        haystack.find(needle)
    }
}

pub(crate) fn parse_size_prefix(text: &str) -> Option<u64> {
    let mut num_end = 0usize;
    let mut saw_digit = false;
    for (idx, ch) in text.char_indices() {
        if ch.is_ascii_digit() {
            saw_digit = true;
            num_end = idx + ch.len_utf8();
        } else if matches!(ch, '.' | ',') && saw_digit {
            num_end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    if !saw_digit {
        return None;
    }
    let clean_num = text[..num_end].replace(',', "");
    let n: f64 = clean_num.parse().ok()?;
    let unit_tail = text[num_end..].trim_start();
    let unit_end = unit_tail
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_alphabetic())
        .map(|(i, _)| i)
        .unwrap_or(unit_tail.len());
    let unit = unit_tail[..unit_end].to_ascii_uppercase();
    let mul = match unit.as_str() {
        "B" | "BYTES" => 1.0,
        "K" | "KB" | "KIB" => 1024.0,
        "M" | "MB" | "MIB" => 1024.0_f64.powi(2),
        "G" | "GB" | "GIB" => 1024.0_f64.powi(3),
        "T" | "TB" | "TIB" => 1024.0_f64.powi(4),
        "P" | "PB" | "PIB" => 1024.0_f64.powi(5),
        _ => return None,
    };
    Some((n * mul).round() as u64)
}
