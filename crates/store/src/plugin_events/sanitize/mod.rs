use serde_json::Value;

const MAX_ERROR_CHARS: usize = 256;
const DEFAULT_ERROR: &str = "plugin delivery failed";
const REDACTED: &str = "[REDACTED]";
const SENSITIVE_SEGS: &[&str] = &[
    "token",
    "tokens",
    "key",
    "keys",
    "secret",
    "secrets",
    "pass",
    "password",
    "passwords",
    "passwd",
    "pwd",
    "passkey",
    "passphrase",
    "passcode",
    "apikey",
    "cookie",
    "cookies",
    "auth",
    "authorization",
    "credential",
    "credentials",
    "signature",
    "sig",
    "session",
    "jwt",
    "bearer",
];
const SAFE_PASS: &[&str] = &["passage", "passenger", "passengers", "passive", "passively"];
const SAFE_AUTH: &[&str] = &[
    "author",
    "authors",
    "authored",
    "authoring",
    "authority",
    "authorities",
];
const TRAPS: &[&str] = &[
    "wasm backtrace",
    "wasm trap",
    "unreachable",
    "stack backtrace",
    "panicked at",
    "wasmtime::",
];

/// Scrubs credentials, traps/backtraces, and excess whitespace from a delivery
/// error, failing closed on obfuscated/malformed syntax and bounding output to
/// at most 256 Unicode scalar values.
pub fn sanitize_delivery_error(raw: &str) -> String {
    let mut cur = sanitize_once(raw);
    for _ in 0..4 {
        let next = sanitize_once(&cur);
        if next == cur {
            return cur;
        }
        cur = next;
    }
    cur
}

fn sanitize_once(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    let decoded = decode_escapes(raw);
    let dec_lower = decoded.to_ascii_lowercase();
    if TRAPS
        .iter()
        .any(|k| lower.contains(k) || dec_lower.contains(k))
    {
        return DEFAULT_ERROR.to_string();
    }
    let trimmed = raw.trim();
    let out = match try_full_json(trimmed, 0) {
        Some((json_out, true)) => json_out,
        Some((json_out, false)) => json_out.split_whitespace().collect::<Vec<_>>().join(" "),
        None => sanitize_non_json(trimmed, 0)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    };
    if out.is_empty() {
        return DEFAULT_ERROR.to_string();
    }
    if out.chars().count() <= MAX_ERROR_CHARS {
        return out;
    }
    let end = out
        .char_indices()
        .nth(MAX_ERROR_CHARS - 3)
        .map_or(out.len(), |(i, _)| i);
    format!("{}...", &out[..end])
}

fn try_full_json(input: &str, depth: usize) -> Option<(String, bool)> {
    if depth >= 8 || !(input.starts_with('{') || input.starts_with('[')) {
        return None;
    }
    let mut val = serde_json::from_str::<Value>(input).ok()?;
    if !sanitize_json_val(&mut val, depth) {
        return Some((input.to_string(), true));
    }
    Some((
        serde_json::to_string(&val).unwrap_or_else(|_| REDACTED.into()),
        false,
    ))
}

fn sanitize_json_val(val: &mut Value, depth: usize) -> bool {
    if depth >= 8 {
        *val = Value::String(REDACTED.into());
        return true;
    }
    match val {
        Value::Object(map) => map.iter_mut().fold(false, |changed, (k, v)| {
            if is_sensitive_key(k) {
                if !v.is_null() && v.as_str() != Some(REDACTED) {
                    *v = Value::String(REDACTED.into());
                    return true;
                }
                changed
            } else {
                changed | sanitize_json_val(v, depth + 1)
            }
        }),
        Value::Array(arr) => arr
            .iter_mut()
            .fold(false, |c, v| c | sanitize_json_val(v, depth + 1)),
        Value::String(s) => {
            let clean = try_full_json(s.trim(), depth + 1)
                .map_or_else(|| sanitize_non_json(s, depth + 1), |(o, _)| o);
            (clean != *s).then(|| *s = clean).is_some()
        }
        _ => false,
    }
}

fn sanitize_non_json(input: &str, depth: usize) -> String {
    if ["\\u", "\\U", "\\\"", "\\'", "\\/"]
        .iter()
        .any(|p| input.contains(p))
        && has_cred_clue(&decode_escapes(input))
    {
        return fail_closed_prefix(input);
    }
    let s = sanitize_embedded_json(input, depth);
    let (mut out, mut i) = (String::with_capacity(s.len()), 0usize);
    while i < s.len() {
        let nxt = try_url_userinfo(&s, i, &mut out)
            .or_else(|| try_header(&s, i, &mut out))
            .or_else(|| try_bearer(&s, i, &mut out))
            .or_else(|| try_kv(&s, i, &mut out))
            .or_else(|| try_quoted_kv(&s, i, &mut out))
            .or_else(|| try_hex_addr(&s, i, &mut out));
        if let Some(next_i) = nxt {
            i = next_i;
        } else {
            let len = s[i..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&s[i..i + len]);
            i += len;
        }
    }
    let unbal = b"\"'"
        .iter()
        .any(|q| out.bytes().filter(|b| b == q).count() % 2 == 1);
    if unbal && has_cred_clue(&out) {
        return fail_closed_prefix(&out);
    }
    out
}

fn sanitize_embedded_json(input: &str, depth: usize) -> String {
    if depth >= 8 || !input.contains('{') {
        return input.to_string();
    }
    let (mut out, mut i, mut probes) = (String::with_capacity(input.len()), 0usize, 0usize);
    while i < input.len() {
        if input.as_bytes()[i] == b'{' && probes < 6 {
            probes += 1;
            let mut de = serde_json::Deserializer::from_str(&input[i..]).into_iter::<Value>();
            if let Some(Ok(mut val)) = de.next() {
                let end = i + de.byte_offset();
                if sanitize_json_val(&mut val, depth + 1) {
                    out.push_str(&serde_json::to_string(&val).unwrap_or_else(|_| REDACTED.into()));
                } else {
                    out.push_str(&input[i..end]);
                }
                i = end;
                continue;
            }
        }
        let len = input[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&input[i..i + len]);
        i += len;
    }
    out
}

fn try_url_userinfo(s: &str, i: usize, out: &mut String) -> Option<usize> {
    if !s[i..].starts_with("://") {
        return None;
    }
    let start = i + 3;
    let end = start
        + s[start..]
            .find(|c: char| c.is_whitespace() || "/?#\"')]}".contains(c))
            .unwrap_or(s.len() - start);
    let at = s[start..end].rfind('@').filter(|&p| p > 0)?;
    out.push_str(&format!("://{REDACTED}{}", &s[start + at..end]));
    Some(end)
}

fn try_header(s: &str, i: usize, out: &mut String) -> Option<usize> {
    let k_end = scan_key(s, i)?;
    if s.as_bytes().get(k_end) != Some(&b':')
        || s[k_end..].starts_with("://")
        || !is_sensitive_key(&s[i..k_end])
    {
        return None;
    }
    out.push_str(&format!("{}: {REDACTED}", &s[i..k_end]));
    Some(consume_value(s, k_end + 1, true))
}

fn try_bearer(s: &str, i: usize, out: &mut String) -> Option<usize> {
    if (i > 0 && is_ident(s.as_bytes()[i - 1], false))
        || !s[i..].get(..6)?.eq_ignore_ascii_case("bearer")
    {
        return None;
    }
    let after = i + 6;
    let c = s[after..].chars().next()?;
    if !c.is_whitespace() && !":=/-".contains(c) {
        return None;
    }
    out.push_str(&format!("{} {REDACTED}", &s[i..after]));
    Some(consume_value(s, after + c.len_utf8(), false))
}

fn try_kv(s: &str, i: usize, out: &mut String) -> Option<usize> {
    let mut eq = scan_key(s, i)?;
    let k_end = eq;
    while eq < s.len() && horiz_ws(s, eq) {
        eq += s[eq..].chars().next().map_or(1, char::len_utf8);
    }
    if s.as_bytes().get(eq) != Some(&b'=') || !is_sensitive_key(&s[i..k_end]) {
        return None;
    }
    out.push_str(&s[i..eq + 1]);
    out.push_str(REDACTED);
    if s[eq + 1..].starts_with("\n\n") || s[eq + 1..].starts_with("\r\n\r\n") {
        return Some(eq + 1);
    }
    Some(consume_value(s, eq + 1, false))
}

fn try_quoted_kv(s: &str, i: usize, out: &mut String) -> Option<usize> {
    let (b, q) = (s.as_bytes(), s.as_bytes()[i]);
    if !matches!(q, b'"' | b'\'') {
        return None;
    }
    let mut k_end = i + 1;
    while k_end < b.len() && is_ident(b[k_end], true) {
        k_end += 1;
    }
    if b.get(k_end) != Some(&q) || !is_sensitive_key(&s[i + 1..k_end]) {
        return None;
    }
    let mut col = k_end + 1;
    while col < b.len() && horiz_ws(s, col) {
        col += s[col..].chars().next().map_or(1, char::len_utf8);
    }
    if b.get(col) != Some(&b':') {
        return None;
    }
    let mut v = col + 1;
    while v < b.len() && horiz_ws(s, v) {
        v += s[v..].chars().next().map_or(1, char::len_utf8);
    }
    if s[v..].starts_with("null")
        && s[v + 4..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric())
    {
        return None;
    }
    out.push_str(&format!("{} {REDACTED}", &s[i..col + 1]));
    Some(s.len())
}

fn try_hex_addr(s: &str, i: usize, out: &mut String) -> Option<usize> {
    if (i > 0 && is_ident(s.as_bytes()[i - 1], false))
        || !(s[i..].starts_with("0x") || s[i..].starts_with("0X"))
    {
        return None;
    }
    let mut end = i + 2;
    while end < s.len() && s.as_bytes()[end].is_ascii_hexdigit() {
        end += 1;
    }
    (end - (i + 2) >= 4).then(|| {
        out.push_str("[addr]");
        end
    })
}

fn consume_value(s: &str, mut pos: usize, header: bool) -> usize {
    let b = s.as_bytes();
    let seps: &[u8] = if header { b"" } else { b":=/-" };
    while pos < b.len() && (horiz_ws(s, pos) || seps.contains(&b[pos])) {
        pos += s[pos..].chars().next().map_or(1, char::len_utf8);
    }
    let nl = if s[pos..].starts_with("\r\n") {
        2
    } else {
        usize::from(b.get(pos) == Some(&b'\n'))
    };
    if nl > 0 {
        let next = pos + nl;
        let trim = s[next..]
            .bytes()
            .take_while(|&c| c == b' ' || c == b'\t')
            .count();
        let next_is_header = scan_key(s, next + trim).is_some_and(|ke| b.get(ke) == Some(&b':'));
        if !header || (!matches!(b.get(next), None | Some(b'\n' | b'\r')) && !next_is_header) {
            pos = next;
        }
    }
    if s[pos..].starts_with(REDACTED) {
        return pos + REDACTED.len();
    }
    if !header && matches!(b.get(pos), Some(b'"' | b'\'')) {
        let q = b[pos];
        let rest_line = s
            .get(pos + 2..)
            .and_then(|r| r.split('\n').next())
            .unwrap_or("");
        return if b.get(pos + 1) == Some(&q) && !rest_line.contains(['"', '\'', '{', '[']) {
            pos + 2
        } else {
            s.len()
        };
    }
    if matches!(b.get(pos), Some(b'{' | b'[')) {
        return s.len();
    }
    if header {
        return s[pos..].find('\n').map_or(s.len(), |r| pos + r);
    }
    while let Some(ch) = s[pos..].chars().next() {
        if ch.is_whitespace() || "&;,)]}".contains(ch) {
            break;
        }
        if "\"'{[".contains(ch) {
            return s.len();
        }
        pos += ch.len_utf8();
    }
    pos
}

fn scan_key(s: &str, i: usize) -> Option<usize> {
    let b = s.as_bytes();
    if i >= b.len() || (i > 0 && is_ident(b[i - 1], false)) || !b[i].is_ascii_alphabetic() {
        return None;
    }
    let mut end = i;
    while end < b.len() && is_ident(b[end], true) {
        end += 1;
    }
    Some(end)
}

fn is_ident(b: u8, dot: bool) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || (dot && b == b'.')
}
fn horiz_ws(s: &str, i: usize) -> bool {
    s[i..]
        .chars()
        .next()
        .is_some_and(|c| c != '\n' && c != '\r' && c.is_whitespace())
}
fn fail_closed_prefix(raw: &str) -> String {
    raw.to_ascii_lowercase().find("authorization:").map_or_else(
        || REDACTED.into(),
        |p| format!("{}: {REDACTED}", &raw[p..p + 13]),
    )
}

fn has_cred_clue(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    (l.contains("://") && l.contains('@'))
        || l.contains("bearer")
        || l.contains("authorization")
        || l.contains("cookie")
        || s.split(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '_' | '-' | '.'))
            .any(is_sensitive_key)
}

fn is_sensitive_key(ident: &str) -> bool {
    if ident.is_empty() {
        return false;
    }
    let chars: Vec<char> = ident.chars().collect();
    let (mut seg, mut found) = (String::new(), false);
    let check = |w: &str| {
        SENSITIVE_SEGS.contains(&w)
            || (w.starts_with("pass") && !SAFE_PASS.contains(&w))
            || (w.starts_with("auth") && !SAFE_AUTH.contains(&w))
    };
    for (idx, &ch) in chars.iter().enumerate() {
        let camel = idx > 0
            && !seg.is_empty()
            && (((chars[idx - 1].is_ascii_lowercase() || chars[idx - 1].is_ascii_digit())
                && ch.is_ascii_uppercase())
                || (chars[idx - 1].is_ascii_uppercase()
                    && ch.is_ascii_uppercase()
                    && chars.get(idx + 1).is_some_and(char::is_ascii_lowercase)));
        if matches!(ch, '_' | '-' | '.') || camel {
            found |= !seg.is_empty() && check(&seg);
            seg.clear();
        }
        if !matches!(ch, '_' | '-' | '.') {
            seg.push(ch.to_ascii_lowercase());
        }
    }
    found || (!seg.is_empty() && check(&seg))
}

fn decode_escapes(s: &str) -> String {
    let (mut out, b, mut i) = (String::with_capacity(s.len()), s.as_bytes(), 0usize);
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            if matches!(b[i + 1], b'u' | b'U')
                && i + 6 <= b.len()
                && let Some(ch) = std::str::from_utf8(&b[i + 2..i + 6])
                    .ok()
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .and_then(char::from_u32)
            {
                out.push(ch);
                i += 6;
                continue;
            }
            if matches!(b[i + 1], b'"' | b'\'' | b'/' | b'\\') {
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
        }
        let len = s[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&s[i..i + len]);
        i += len;
    }
    out
}
