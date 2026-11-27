use domain::Site;
use scraper::{ElementRef, Html, Selector};

use crate::profile::{Field, Profile};
use crate::tz::parse_site_datetime;

const FREE_2X_SELECTOR: &str = "img.pro_free2up, [class*='free2up'], img[alt*='2X免费'], img[title*='2X免费'], img[alt*='2X Free'], img[title*='2X Free'], img[alt*='2xFree'], img[title*='2xFree'], font.twoupfree";
const EXPLICIT_PROMO_ATTR_SELECTOR: &str = "img.pro_free[title], img.pro_free2up[title], [class*='pro_free'][title], [class*='free2up'][title], font.free[title], font.twoupfree[title], span.free[title], time.free-until[datetime], [data-free-until]";
const TEXT_OR_ATTR_PROMO_SELECTOR: &str = "span[title], font[title], time[datetime], font, span, b";

pub(crate) fn detect_free_2x(item: ElementRef<'_>) -> bool {
    let Ok(sel) = Selector::parse(FREE_2X_SELECTOR) else {
        return false;
    };
    item.select(&sel).next().is_some()
}

pub(crate) fn extract_free_until(
    item: ElementRef<'_>,
    site: &Site,
    profile: &Profile,
    free: bool,
    upload_time_raw: Option<&str>,
    now: i64,
) -> Option<i64> {
    if !free {
        return None;
    }
    let upload_trim = upload_time_raw.map(str::trim).filter(|s| !s.is_empty());

    // 1. Direct attributes on the matched free field element (or siblings/parent container).
    if let Some(field) = profile.fields.free.as_ref()
        && let Ok(free_sel) = Selector::parse(&field.selector)
    {
        for el in item.select(&free_sel) {
            for attr in ["data-free-until", "datetime", "title"] {
                if let Some(ts) = el
                    .value()
                    .attr(attr)
                    .filter(|raw| upload_trim.is_none_or(|u| u != raw.trim()))
                    .and_then(|raw| parse_promo_value(raw, &site.timezone, now))
                {
                    return Some(ts);
                }
            }
            if let Some(parent) = el.parent().and_then(ElementRef::wrap)
                && let Some(ts) = find_promo_in_subtree(parent, &site.timezone, upload_trim, now)
            {
                return Some(ts);
            }
        }
    }

    // 2. Known promo classes with explicit timestamp/countdown attributes.
    if let Ok(sel) = Selector::parse(EXPLICIT_PROMO_ATTR_SELECTOR) {
        for el in item.select(&sel) {
            for attr in ["data-free-until", "datetime", "title"] {
                if let Some(ts) = el
                    .value()
                    .attr(attr)
                    .filter(|raw| upload_trim.is_none_or(|u| u != raw.trim()))
                    .and_then(|raw| parse_promo_value(raw, &site.timezone, now))
                {
                    return Some(ts);
                }
            }
        }
    }

    // 3. Elements carrying explicit promo wording (`剩余`, `限时`, `Free`, `免费`, `优惠`, `到期`).
    find_promo_in_subtree(item, &site.timezone, upload_trim, now)
}

fn find_promo_in_subtree(
    root: ElementRef<'_>,
    site_timezone: &str,
    upload_trim: Option<&str>,
    now: i64,
) -> Option<i64> {
    let Ok(sel) = Selector::parse(TEXT_OR_ATTR_PROMO_SELECTOR) else {
        return None;
    };
    for el in root.select(&sel) {
        let visible = el.text().collect::<String>();
        let trimmed_text = visible.trim();
        let has_promo_cue = contains_promo_cue(trimmed_text)
            || el.value().attr("title").is_some_and(contains_promo_cue)
            || el
                .value()
                .attr("class")
                .is_some_and(|c| c.contains("free") || c.contains("promo"));
        if !has_promo_cue {
            continue;
        }
        for attr in ["data-free-until", "datetime", "title"] {
            if let Some(raw) = el.value().attr(attr) {
                let candidate = raw.trim();
                if upload_trim.is_some_and(|u| u == candidate) {
                    continue;
                }
                if let Some(ts) = parse_promo_value(candidate, site_timezone, now) {
                    return Some(ts);
                }
            }
        }
        if let Some(ts) = parse_promo_value(trimmed_text, site_timezone, now) {
            return Some(ts);
        }
    }
    None
}

fn contains_promo_cue(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    text.contains("剩余")
        || text.contains("限时")
        || text.contains("免费")
        || text.contains("优惠")
        || text.contains("到期")
        || lower.contains("free")
        || lower.contains("until")
        || lower.contains("2x")
}

pub(crate) fn parse_promo_value(raw: &str, site_timezone: &str, now: i64) -> Option<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(ts) = parse_site_datetime(trimmed, site_timezone) {
        return Some(ts);
    }
    if let Some(embedded) = extract_embedded_datetime(trimmed)
        && let Some(ts) = parse_site_datetime(embedded, site_timezone)
    {
        return Some(ts);
    }
    parse_relative_duration_secs(trimmed).map(|secs| now.saturating_add(secs))
}

fn extract_embedded_datetime(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    for start in 0..bytes.len() {
        if start + 16 <= bytes.len()
            && bytes[start..start + 4].iter().all(u8::is_ascii_digit)
            && bytes[start + 4] == b'-'
            && bytes[start + 5..start + 7].iter().all(u8::is_ascii_digit)
            && bytes[start + 7] == b'-'
        {
            let tail = &raw[start..];
            let end = tail
                .find(|c: char| {
                    !c.is_ascii_digit() && !matches!(c, '-' | ':' | ' ' | 'T' | 'Z' | '+')
                })
                .unwrap_or(tail.len());
            let candidate = tail[..end].trim();
            if candidate.len() >= 16 {
                return Some(candidate);
            }
        }
    }
    None
}

fn parse_relative_duration_secs(raw: &str) -> Option<i64> {
    let clean = raw
        .trim()
        .trim_start_matches("剩余")
        .trim_start_matches([':', '：', ' ']);
    let mut total = 0i64;
    let mut matched = false;
    let mut num = String::new();
    let chars: Vec<char> = clean.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_ascii_digit() {
            num.push(ch);
            i += 1;
            continue;
        }
        if !num.is_empty() {
            let val = num.parse::<i64>().ok()?;
            num.clear();
            let rest: String = chars[i..].iter().collect();
            let unit_secs = if rest.starts_with("天")
                || rest.starts_with("日")
                || rest.starts_with('d')
                || rest.starts_with('D')
            {
                Some(86_400)
            } else if rest.starts_with("小时") {
                i += 1;
                Some(3600)
            } else if rest.starts_with("时") || rest.starts_with('h') || rest.starts_with('H') {
                Some(3600)
            } else if rest.starts_with("分钟") {
                i += 1;
                Some(60)
            } else if rest.starts_with("分") || rest.starts_with('m') || rest.starts_with('M') {
                Some(60)
            } else if rest.starts_with("秒") || rest.starts_with('s') || rest.starts_with('S') {
                Some(1)
            } else {
                None
            };
            if let Some(mul) = unit_secs {
                total = total.saturating_add(val.saturating_mul(mul));
                matched = true;
            }
        }
        i += 1;
    }
    matched.then_some(total)
}

#[allow(dead_code)]
pub(crate) fn _check_html_field_anchor(_: &Html, _: &Field) {}
