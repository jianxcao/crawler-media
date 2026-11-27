const MAX_REASON_CHARS: usize = 200;

const SENSITIVE_KEYS: &[&str] = &[
    "passkey",
    "token",
    "cookie",
    "key",
    "auth",
    "secret",
    "password",
    "pass",
    "passwd",
    "pwd",
    "apikey",
    "api_key",
    "x-api-key",
    "x_api_key",
    "x-auth-token",
    "x_auth_token",
    "client_secret",
    "access_token",
    "refresh_token",
    "set-cookie",
    "credential",
    "credentials",
    "signature",
    "sig",
    "sign",
    "authkey",
    "torrent_pass",
    "downhash",
    "rsskey",
    "rss_key",
    "download_token",
    "session",
    "authorization",
    "reauth_token",
    "consent_token",
];

const HEADER_KEYS: &[&str] = &[
    "cookie",
    "set-cookie",
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "x-auth-token",
    "passkey",
    "password",
    "api_key",
    "apikey",
    "secret",
    "token",
];

pub fn sanitize_reason(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("wasm backtrace")
        || lower.contains("wasm trap")
        || lower.contains("unreachable")
        || lower.contains("stack backtrace")
        || lower.contains("panicked at")
        || lower.contains("wasmtime::")
    {
        return "plugin execution error".to_string();
    }

    let mut out = redact_url_userinfo(raw);
    out = redact_pt_url_paths(&out);
    for key in SENSITIVE_KEYS {
        out = redact_kv_ci(&out, key);
        out = redact_kv_pct_ci(&out, key);
        out = redact_json_kv_ci(&out, key);
    }
    for key in HEADER_KEYS {
        out = redact_header_ci(&out, key);
    }
    out = redact_bearer(&out);
    out = redact_hex_addrs(&out);

    let normalized: String = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return "plugin execution error".to_string();
    }
    truncate_chars(trimmed, MAX_REASON_CHARS)
}

pub fn redact_urls(raw: &str) -> String {
    let scrubbed = strip_all_http_urls(raw);
    sanitize_reason(&scrubbed)
}

fn strip_all_http_urls(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < input.len() {
        let next_http = lower[cursor..].find("http://");
        let next_https = lower[cursor..].find("https://");
        let rel = match (next_http, next_https) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        let abs = cursor + rel;
        out.push_str(&input[cursor..abs]);
        out.push_str("[redacted-url]");
        let end = input[abs..]
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '"' | '\'' | ')' | ']' | '}' | '<' | '>' | ';')
            })
            .map_or(input.len(), |i| abs + i);
        cursor = end;
    }
    out.push_str(&input[cursor..]);
    out
}

fn redact_pt_url_paths(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < input.len() {
        let next_http = lower[cursor..].find("http://");
        let next_https = lower[cursor..].find("https://");
        let rel = match (next_http, next_https) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        let abs = cursor + rel;
        let url_end = input[abs..]
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '"' | '\'' | ')' | ']' | '}' | '<' | '>' | ';')
            })
            .map_or(input.len(), |i| abs + i);
        out.push_str(&input[cursor..abs]);
        out.push_str(&redact_single_url_path(&input[abs..url_end]));
        cursor = url_end;
    }
    out.push_str(&input[cursor..]);
    out
}

fn redact_single_url_path(url: &str) -> String {
    let Some(scheme_sep) = url.find("://") else {
        return url.to_string();
    };
    let after_scheme = scheme_sep + 3;
    let Some(slash_rel) = url[after_scheme..].find('/') else {
        return url.to_string();
    };
    let path_start = after_scheme + slash_rel;
    let path_end = url[path_start..]
        .find(['?', '#'])
        .map_or(url.len(), |i| path_start + i);
    let path = &url[path_start..path_end];
    let segs: Vec<&str> = path.split('/').collect();
    let mut new_segs = Vec::with_capacity(segs.len());
    let mut prev_secret_dir = false;
    let mut slack_services_step: u8 = 0;
    for seg in segs {
        let lower = seg.to_ascii_lowercase();
        if slack_services_step == 1
            && !seg.is_empty()
            && seg.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            slack_services_step = 2;
            new_segs.push(seg.to_string());
            continue;
        }
        if slack_services_step == 2
            && !seg.is_empty()
            && seg.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            slack_services_step = 3;
            new_segs.push(seg.to_string());
            continue;
        }
        if slack_services_step == 3 && seg.len() >= 8 {
            slack_services_step = 0;
            new_segs.push("[REDACTED]".to_string());
            continue;
        }
        slack_services_step = u8::from(lower == "services");

        if seg.len() > 4
            && let Some(prefix) = seg.get(..3)
            && prefix.eq_ignore_ascii_case("bot")
            && let Some((bot_id, bot_secret)) = seg.get(3..).and_then(|rest| rest.split_once(':'))
            && !bot_id.is_empty()
            && bot_id.bytes().all(|b| b.is_ascii_digit())
            && !bot_secret.is_empty()
        {
            new_segs.push(format!("{prefix}{bot_id}:[REDACTED]"));
            prev_secret_dir = false;
            continue;
        }

        if prev_secret_dir {
            let is_structural = matches!(
                lower.as_str(),
                "download"
                    | "dl"
                    | "get"
                    | "feed"
                    | "torrents"
                    | "torrent"
                    | "index.php"
                    | "download.php"
                    | "torrents.php"
            ) || (!seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()));
            if is_structural {
                new_segs.push(seg.to_string());
                continue;
            }
            if seg.len() >= 8 {
                new_segs.push("[REDACTED]".to_string());
                prev_secret_dir = false;
                continue;
            }
        }
        prev_secret_dir = matches!(
            lower.as_str(),
            "rss" | "passkey" | "torznab" | "webhook" | "webhooks" | "hook" | "hooks"
        );
        new_segs.push(redact_pt_id_dot_passkey_seg(seg));
    }
    format!(
        "{}{}{}",
        &url[..path_start],
        new_segs.join("/"),
        &url[path_end..]
    )
}

fn redact_pt_id_dot_passkey_seg(seg: &str) -> String {
    let Some((digits, rest)) = seg.split_once('.') else {
        return seg.to_string();
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) || rest.is_empty() {
        return seg.to_string();
    }
    let lower_rest = rest.to_ascii_lowercase();
    if matches!(
        lower_rest.as_str(),
        "torrent" | "rss" | "xml" | "json" | "html" | "php" | "txt"
    ) {
        return seg.to_string();
    }
    if let Some(secret_mid) = lower_rest.strip_suffix(".torrent")
        && !secret_mid.is_empty()
    {
        return format!("{digits}.[REDACTED].torrent");
    }
    format!("{digits}.[REDACTED]")
}

fn redact_kv_pct_ci(input: &str, key: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let needle = format!("{key}%3d");
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(pos) = lower[cursor..].find(&needle) {
        let abs = cursor + pos;
        if !boundary_before_ok(input, abs) {
            out.push_str(&input[cursor..abs + needle.len()]);
            cursor = abs + needle.len();
            continue;
        }
        out.push_str(&input[cursor..abs + key.len()]);
        out.push_str("=[REDACTED]");
        let val_start = abs + needle.len();
        let val_end = lower[val_start..]
            .find("%26")
            .into_iter()
            .chain(input[val_start..].find(|c: char| {
                c.is_whitespace() || matches!(c, '&' | ';' | '"' | '\'' | ')' | ']' | '}' | '#')
            }))
            .min()
            .map_or(input.len(), |i| val_start + i);
        cursor = val_end;
    }
    out.push_str(&input[cursor..]);
    out
}

fn redact_url_userinfo(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(pos) = input[cursor..].find("://") {
        let scheme_end = cursor + pos + 3;
        let auth_rel_end = input[scheme_end..]
            .find(|c: char| {
                c.is_whitespace()
                    || matches!(
                        c,
                        '/' | '?' | '#' | '"' | '\'' | ')' | ']' | '}' | '<' | '>'
                    )
            })
            .unwrap_or(input.len() - scheme_end);
        let auth_end = scheme_end + auth_rel_end;
        let authority = &input[scheme_end..auth_end];
        if let Some(at_pos) = authority.rfind('@')
            && at_pos > 0
        {
            out.push_str(&input[cursor..scheme_end]);
            out.push_str("[REDACTED]");
            out.push_str(&authority[at_pos..]);
            cursor = auth_end;
            continue;
        }
        out.push_str(&input[cursor..scheme_end]);
        cursor = scheme_end;
    }
    out.push_str(&input[cursor..]);
    out
}

fn boundary_before_ok(input: &str, abs: usize) -> bool {
    abs == 0
        || !input[..abs]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn redact_kv_ci(input: &str, key: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let needle = format!("{key}=");
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(pos) = lower[cursor..].find(&needle) {
        let abs = cursor + pos;
        if !boundary_before_ok(input, abs) {
            out.push_str(&input[cursor..abs + needle.len()]);
            cursor = abs + needle.len();
            continue;
        }
        out.push_str(&input[cursor..abs + needle.len()]);
        out.push_str("[REDACTED]");
        let val_start = abs + needle.len();
        let val_end = input[val_start..]
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '&' | ';' | '"' | '\'' | ')' | ']' | '}')
            })
            .map_or(input.len(), |i| val_start + i);
        cursor = val_end;
    }
    out.push_str(&input[cursor..]);
    out
}

fn redact_header_ci(input: &str, key: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let needle = format!("{key}:");
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(pos) = lower[cursor..].find(&needle) {
        let abs = cursor + pos;
        if !boundary_before_ok(input, abs) {
            out.push_str(&input[cursor..abs + needle.len()]);
            cursor = abs + needle.len();
            continue;
        }
        out.push_str(&input[cursor..abs + needle.len()]);
        let after_colon = abs + needle.len();
        let ws_len = input[after_colon..]
            .bytes()
            .take_while(|b| matches!(b, b' ' | b'\t'))
            .count();
        out.push_str(&input[after_colon..after_colon + ws_len]);
        let val_start = after_colon + ws_len;
        if val_start >= input.len() {
            cursor = val_start;
            break;
        }
        out.push_str("[REDACTED]");
        cursor = find_header_val_end(input, val_start);
    }
    out.push_str(&input[cursor..]);
    out
}

fn find_header_val_end(input: &str, start: usize) -> usize {
    let bytes = input.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        if matches!(b, b'\r' | b'\n' | b'{' | b'}' | b']' | b')' | b'"' | b'\'') {
            return i;
        }
        if b == b'\\' && bytes.get(i + 1) == Some(&b'"') {
            return i;
        }
        i += 1;
    }
    bytes.len()
}

fn redact_json_kv_ci(input: &str, key: &str) -> String {
    let mid = redact_json_pattern(input, &format!("\"{key}\""), "\"");
    redact_json_pattern(&mid, &format!("\\\"{key}\\\""), "\\\"")
}

fn redact_json_pattern(input: &str, key_pat: &str, val_quote: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(pos) = lower[cursor..].find(key_pat) {
        let abs = cursor + pos;
        let after_key = abs + key_pat.len();
        let ws1 = count_ws(&input[after_key..]);
        let colon_idx = after_key + ws1;
        if input.as_bytes().get(colon_idx) != Some(&b':') {
            out.push_str(&input[cursor..after_key]);
            cursor = after_key;
            continue;
        }
        let ws2 = count_ws(&input[colon_idx + 1..]);
        let val_open = colon_idx + 1 + ws2;
        if !input[val_open..].starts_with(val_quote) {
            out.push_str(&input[cursor..colon_idx + 1]);
            cursor = colon_idx + 1;
            continue;
        }
        let val_inner = val_open + val_quote.len();
        if let Some(close_rel) = find_closing_val_quote(&input[val_inner..], val_quote) {
            out.push_str(&input[cursor..val_inner]);
            out.push_str("[REDACTED]");
            cursor = val_inner + close_rel;
        } else {
            out.push_str(&input[cursor..val_inner]);
            out.push_str("[REDACTED]");
            cursor = input.len();
        }
    }
    out.push_str(&input[cursor..]);
    out
}

fn find_closing_val_quote(rest: &str, val_quote: &str) -> Option<usize> {
    let mut i = 0;
    if val_quote == "\"" {
        while i < rest.len() {
            let ch = rest[i..].chars().next()?;
            if ch == '\\' {
                i += 1;
                if let Some(escaped) = rest[i..].chars().next() {
                    i += escaped.len_utf8();
                }
            } else if ch == '"' {
                return Some(i);
            } else {
                i += ch.len_utf8();
            }
        }
        None
    } else {
        while i < rest.len() {
            if rest[i..].starts_with(r#"\\\\"#) || rest[i..].starts_with(r#"\\\""#) {
                i += 4;
            } else if rest[i..].starts_with(r#"\\""#) {
                return Some(i + 2);
            } else if rest[i..].starts_with(r#"\""#) {
                return Some(i);
            } else {
                i += rest[i..].chars().next().map_or(1, char::len_utf8);
            }
        }
        None
    }
}

fn count_ws(s: &str) -> usize {
    s.bytes().take_while(|b| b.is_ascii_whitespace()).count()
}

fn redact_bearer(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let needle = "bearer ";
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(pos) = lower[cursor..].find(needle) {
        let abs = cursor + pos;
        out.push_str(&input[cursor..abs + needle.len()]);
        out.push_str("[REDACTED]");
        let val_start = abs + needle.len();
        let val_end = input[val_start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ';' | ',' | '\\'))
            .map_or(input.len(), |i| val_start + i);
        cursor = val_end;
    }
    out.push_str(&input[cursor..]);
    out
}

fn redact_hex_addrs(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(pos) = input[cursor..].find("0x") {
        let abs = cursor + pos;
        let hex_start = abs + 2;
        let hex_len = input[hex_start..]
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .count();
        if hex_len >= 4 {
            out.push_str(&input[cursor..abs]);
            out.push_str("[addr]");
            cursor = hex_start + hex_len;
        } else {
            out.push_str(&input[cursor..hex_start]);
            cursor = hex_start;
        }
    }
    out.push_str(&input[cursor..]);
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut end = 0;
    for (idx, (byte_idx, _)) in s.char_indices().enumerate() {
        if idx == max {
            end = byte_idx;
            break;
        }
    }
    if end == 0 {
        s.to_string()
    } else {
        format!("{}...", &s[..end])
    }
}
