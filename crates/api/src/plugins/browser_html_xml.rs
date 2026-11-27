pub(super) fn local_name(tag_name: &[u8], is_xhtml: bool) -> &[u8] {
    if is_xhtml {
        tag_name.rsplit(|&b| b == b':').next().unwrap_or(tag_name)
    } else {
        tag_name
    }
}

pub(super) fn detect_prefixed_xhtml_root(raw: &str) -> bool {
    let head = &raw[..raw.len().min(4096)];
    let bytes = head.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let Some(rel_lt) = bytes[i..].iter().position(|&b| b == b'<') else {
            return false;
        };
        let lt = i + rel_lt;
        if bytes[lt..].starts_with(b"<!--") {
            i = scan_xml_comment_end(bytes, lt);
            continue;
        }
        if matches!(bytes.get(lt + 1), Some(b'?' | b'!')) {
            i = if bytes.get(lt + 1) == Some(&b'?') {
                scan_xml_pi_end(bytes, lt)
            } else {
                scan_xml_decl_end(bytes, lt)
            };
            continue;
        }
        if bytes.get(lt + 1).is_some_and(u8::is_ascii_alphabetic) {
            let Some(close) = scan_prescan_tag_close(bytes, lt + 1) else {
                return false;
            };
            let tag = &head[lt..close];
            let mut name_end = 1usize;
            while name_end < tag.len()
                && !tag.as_bytes()[name_end].is_ascii_whitespace()
                && !matches!(tag.as_bytes()[name_end], b'/' | b'>')
            {
                name_end += 1;
            }
            let tag_name = &tag[1..name_end];
            if !tag_name.contains(':') {
                return false;
            }
            return super::parse_html_attrs(tag.as_bytes(), name_end)
                .iter()
                .any(|a| {
                    a.has_eq
                        && tag[a.name_start..a.name_end]
                            .get(..6)
                            .is_some_and(|p| p.eq_ignore_ascii_case("xmlns:"))
                        && tag[a.val_start..a.val_end]
                            .trim()
                            .eq_ignore_ascii_case("http://www.w3.org/1999/xhtml")
                });
        }
        return false;
    }
    false
}

pub(super) fn scan_xml_pi_end(bytes: &[u8], lt: usize) -> usize {
    let body_start = lt + 2;
    bytes[body_start..]
        .windows(2)
        .position(|w| w == b"?>")
        .map_or(bytes.len(), |rel| body_start + rel + 2)
}

pub(super) fn scan_xml_comment_end(bytes: &[u8], lt: usize) -> usize {
    let body_start = lt + 4;
    bytes[body_start..]
        .windows(3)
        .position(|w| w == b"-->")
        .map_or(bytes.len(), |rel| body_start + rel + 3)
}

pub(super) fn scan_xml_decl_end(bytes: &[u8], lt: usize) -> usize {
    let mut i = lt + 2;
    let mut bracket_depth = 0usize;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        if let Some(q) = quote {
            if bytes[i] == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"<!--") {
            i = scan_xml_comment_end(bytes, i);
            continue;
        }
        if bytes[i..].starts_with(b"<?") {
            i = scan_xml_pi_end(bytes, i);
            continue;
        }
        match bytes[i] {
            b'"' | b'\'' => {
                quote = Some(bytes[i]);
                i += 1;
            }
            b'[' => {
                bracket_depth += 1;
                i += 1;
            }
            b']' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                i += 1;
            }
            b'>' if bracket_depth == 0 => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

fn sniff_utf16_without_bom(body: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    if body.len() < 4 {
        return None;
    }
    let sample = &body[..body.len().min(512)];
    let pairs = sample.len() / 2;
    if pairs == 0 {
        return None;
    }
    let mut even_nul = 0usize;
    let mut odd_nul = 0usize;
    for chunk in sample.as_chunks::<2>().0 {
        if chunk[0] == 0 && chunk[1] != 0 {
            even_nul += 1;
        } else if chunk[1] == 0 && chunk[0] != 0 {
            odd_nul += 1;
        }
    }
    if odd_nul * 3 >= pairs && even_nul == 0 {
        Some(encoding_rs::UTF_16LE)
    } else if even_nul * 3 >= pairs && odd_nul == 0 {
        Some(encoding_rs::UTF_16BE)
    } else {
        None
    }
}

fn is_http_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn split_http_quoted(input: &str, sep: u8) -> Vec<&str> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut in_dquote = false;
    while i < bytes.len() {
        if in_dquote {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if bytes[i] == b'"' {
                in_dquote = false;
            }
            i += 1;
            continue;
        }
        if bytes[i] == b'"' {
            in_dquote = true;
            i += 1;
            continue;
        }
        if bytes[i] == sep {
            out.push(&input[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    out.push(&input[start..]);
    out
}

fn unquote_http_param_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(inner) = trimmed.strip_prefix('"') {
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(escaped) = chars.next() {
                    out.push(escaped);
                }
            } else if c == '"' {
                break;
            } else {
                out.push(c);
            }
        }
        return out.trim().to_string();
    }
    if let Some(inner) = trimmed.strip_prefix('\'') {
        let end = inner.find('\'').unwrap_or(inner.len());
        return inner[..end].trim().to_string();
    }
    trimmed.to_string()
}

fn extract_param_charset(params: &[&str]) -> Option<String> {
    for part in params {
        let Some((name, val)) = part.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("charset") {
            let unq = unquote_http_param_value(val);
            if !unq.is_empty() {
                return Some(unq);
            }
        }
    }
    None
}

fn parse_fetch_content_type(headers: &[(String, String)]) -> (Option<String>, Option<String>) {
    let mut essence: Option<String> = None;
    let mut charset: Option<String> = None;
    for (k, v) in headers {
        if !k.trim().eq_ignore_ascii_case("content-type") {
            continue;
        }
        for candidate in split_http_quoted(v, b',') {
            let parts = split_http_quoted(candidate, b';');
            let Some(first) = parts.first() else {
                continue;
            };
            let ess_raw = first.trim();
            let Some((ty, sub)) = ess_raw.split_once('/') else {
                continue;
            };
            let ty = ty.trim();
            let sub = sub.trim();
            if ty.is_empty()
                || sub.is_empty()
                || (ty == "*" && sub == "*")
                || !ty.bytes().all(is_http_token_byte)
                || !sub.bytes().all(is_http_token_byte)
            {
                continue;
            }
            let next_ess = format!("{}/{}", ty.to_ascii_lowercase(), sub.to_ascii_lowercase());
            let cand_charset = extract_param_charset(&parts[1..]);
            if essence.as_deref() != Some(next_ess.as_str()) {
                essence = Some(next_ess);
                charset = cand_charset;
            } else if charset.is_none() && cand_charset.is_some() {
                charset = cand_charset;
            }
        }
    }
    (essence, charset)
}

fn extract_meta_content_charset(content: &str) -> Option<String> {
    let bytes = content.as_bytes();
    let mut pos = 0usize;
    while pos + 7 <= bytes.len() {
        let Some(rel) = bytes[pos..]
            .windows(7)
            .position(|w| w.eq_ignore_ascii_case(b"charset"))
        else {
            break;
        };
        let mut i = pos + rel + 7;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            pos = i;
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        let q = bytes[i];
        if q == b'"' || q == b'\'' {
            let end = bytes[i + 1..].iter().position(|&b| b == q)?;
            let label = content[i + 1..i + 1 + end].trim();
            return (!label.is_empty()).then(|| label.to_string());
        }
        let end = bytes[i..]
            .iter()
            .position(|&b| b.is_ascii_whitespace() || b == b';')
            .unwrap_or(bytes.len() - i);
        let label = content[i..i + end].trim();
        return (!label.is_empty()).then(|| label.to_string());
    }
    None
}

fn normalize_prescan_encoding(
    enc: &'static encoding_rs::Encoding,
) -> &'static encoding_rs::Encoding {
    if enc == encoding_rs::UTF_16BE || enc == encoding_rs::UTF_16LE {
        encoding_rs::UTF_8
    } else {
        enc
    }
}

fn prescan_xml_decl_encoding(head: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let trimmed_start = head
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(head.len());
    let rest = &head[trimmed_start..];
    if !rest
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case(b"<?xml"))
    {
        return None;
    }
    let pi_end = scan_xml_pi_end(rest, 0);
    let decl_str = String::from_utf8_lossy(&rest[5..pi_end.saturating_sub(2)]);
    for attr in super::parse_html_attrs(decl_str.as_bytes(), 0) {
        if attr.has_eq && decl_str[attr.name_start..attr.name_end].eq_ignore_ascii_case("encoding")
        {
            let label = decl_str[attr.val_start..attr.val_end].trim();
            if let Some(enc) = encoding_rs::Encoding::for_label(label.as_bytes()) {
                return Some(normalize_prescan_encoding(enc));
            }
        }
    }
    None
}

fn scan_prescan_tag_close(head: &[u8], start: usize) -> Option<usize> {
    let mut close = start;
    let mut quote: Option<u8> = None;
    while close < head.len() {
        let b = head[close];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
        } else if b == b'"' || b == b'\'' {
            quote = Some(b);
        } else if b == b'>' {
            return Some(close + 1);
        }
        close += 1;
    }
    None
}

fn parse_meta_tag_encoding(tag_slice: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let tag_lossy = String::from_utf8_lossy(tag_slice);
    let attrs = super::parse_html_attrs(tag_lossy.as_bytes(), 5);
    let mut got_pragma = false;
    let mut need_pragma: Option<bool> = None;
    let mut candidate: Option<&'static encoding_rs::Encoding> = None;
    for a in &attrs {
        if !a.has_eq {
            continue;
        }
        let name = &tag_lossy[a.name_start..a.name_end];
        let val = tag_lossy[a.val_start..a.val_end].trim();
        if name.eq_ignore_ascii_case("charset")
            && candidate.is_none()
            && let Some(enc) = encoding_rs::Encoding::for_label(val.as_bytes())
        {
            candidate = Some(enc);
            need_pragma = Some(false);
        } else if name.eq_ignore_ascii_case("http-equiv")
            && val.eq_ignore_ascii_case("content-type")
        {
            got_pragma = true;
        } else if name.eq_ignore_ascii_case("content")
            && candidate.is_none()
            && let Some(lbl) = extract_meta_content_charset(val)
            && let Some(enc) = encoding_rs::Encoding::for_label(lbl.as_bytes())
        {
            candidate = Some(enc);
            need_pragma = Some(true);
        }
    }
    if let (Some(enc), Some(pragma_req)) = (candidate, need_pragma)
        && (!pragma_req || got_pragma)
    {
        return Some(normalize_prescan_encoding(enc));
    }
    None
}

fn prescan_meta_charset(head: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let mut i = 0usize;
    while i < head.len() {
        let Some(rel_lt) = head[i..].iter().position(|&b| b == b'<') else {
            break;
        };
        let lt = i + rel_lt;
        if head[lt..].starts_with(b"<!--") {
            i = super::scan_html_comment_end(head, lt);
            continue;
        }
        if head
            .get(lt + 1..lt + 5)
            .is_some_and(|m| m.eq_ignore_ascii_case(b"meta"))
            && head
                .get(lt + 5)
                .is_some_and(|b| b.is_ascii_whitespace() || *b == b'/' || *b == b'>')
        {
            let close = scan_prescan_tag_close(head, lt + 5)?;
            if let Some(enc) = parse_meta_tag_encoding(&head[lt..close]) {
                return Some(enc);
            }
            i = close;
            continue;
        }
        if head.get(lt + 1).is_some_and(u8::is_ascii_alphabetic) {
            let mut name_end = lt + 2;
            while name_end < head.len()
                && !head[name_end].is_ascii_whitespace()
                && head[name_end] != b'>'
            {
                name_end += 1;
            }
            i = scan_prescan_tag_close(head, name_end)?;
            continue;
        }
        if head.get(lt + 1) == Some(&b'/') && head.get(lt + 2).is_some_and(u8::is_ascii_alphabetic)
        {
            let mut name_end = lt + 3;
            while name_end < head.len()
                && !head[name_end].is_ascii_whitespace()
                && head[name_end] != b'>'
            {
                name_end += 1;
            }
            i = scan_prescan_tag_close(head, name_end)?;
            continue;
        }
        if matches!(head.get(lt + 1), Some(b'!' | b'?' | b'/')) {
            i = head[lt + 2..]
                .iter()
                .position(|&b| b == b'>')
                .map_or(head.len(), |rel| lt + 2 + rel + 1);
            continue;
        }
        i = lt + 1;
    }
    None
}

pub(crate) fn decode_nav_body_for_sanitize(
    headers: &[(String, String)],
    body: &[u8],
) -> Option<(String, Option<bool>)> {
    let (mime_lower, ct_charset) = parse_fetch_content_type(headers);
    let is_html_mime = match mime_lower.as_deref() {
        None | Some("") => true,
        Some(m) => m.contains("html") || m.contains("xhtml") || m.contains("xml"),
    };
    let explicit_xhtml = match mime_lower.as_deref() {
        Some(m) if m.contains("xhtml") || m.contains("xml") => Some(true),
        Some(m) if m.contains("html") => Some(false),
        _ => None,
    };
    let charset_enc = ct_charset
        .as_deref()
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()));
    let xml_head = &body[..body.len().min(4096)];
    let meta_head = &body[..body.len().min(65_536)];

    let decoded = if let Some((bom_enc, bom_len)) = encoding_rs::Encoding::for_bom(body) {
        let (cow, _) = bom_enc.decode_without_bom_handling(&body[bom_len..]);
        cow.into_owned()
    } else if let Some(enc) = charset_enc {
        let (cow, _) = enc.decode_without_bom_handling(body);
        cow.into_owned()
    } else if is_html_mime && let Some(utf16_enc) = sniff_utf16_without_bom(body) {
        let (cow, _) = utf16_enc.decode_without_bom_handling(body);
        cow.into_owned()
    } else if is_html_mime
        && let Some(prescan_enc) = (explicit_xhtml != Some(false))
            .then(|| prescan_xml_decl_encoding(xml_head))
            .flatten()
            .or_else(|| {
                (explicit_xhtml != Some(true))
                    .then(|| prescan_meta_charset(meta_head))
                    .flatten()
            })
    {
        let (cow, _) = prescan_enc.decode_without_bom_handling(body);
        cow.into_owned()
    } else {
        let (cow, _) = encoding_rs::UTF_8.decode_without_bom_handling(body);
        cow.into_owned()
    };

    if !is_html_mime && !decoded.contains('<') {
        return None;
    }
    Some((decoded, explicit_xhtml))
}

pub(super) fn decode_numeric_entities(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find("&#") {
        out.push_str(&rest[..amp]);
        let after_hash = &rest[amp + 2..];
        if let Some((ch, next_rest)) = parse_numeric_entity(after_hash) {
            out.push(ch);
            rest = next_rest;
            continue;
        }
        out.push_str("&#");
        rest = after_hash;
    }
    out.push_str(rest);
    out
}

fn parse_numeric_entity(after_hash: &str) -> Option<(char, &str)> {
    if let Some(after_x) = after_hash
        .strip_prefix('x')
        .or_else(|| after_hash.strip_prefix('X'))
    {
        let digit_len = after_x.bytes().take_while(u8::is_ascii_hexdigit).count();
        let digits = &after_x[..digit_len];
        if digits.is_empty() {
            return None;
        }
        let trimmed = digits.trim_start_matches('0');
        let norm = if trimmed.is_empty() { "0" } else { trimmed };
        let ch = (norm.len() <= 6)
            .then(|| u32::from_str_radix(norm, 16).ok())
            .flatten()
            .and_then(char::from_u32)?;
        let after_digits = &after_x[digit_len..];
        return Some((ch, after_digits.strip_prefix(';').unwrap_or(after_digits)));
    }
    let digit_len = after_hash.bytes().take_while(u8::is_ascii_digit).count();
    let digits = &after_hash[..digit_len];
    if digits.is_empty() {
        return None;
    }
    let trimmed = digits.trim_start_matches('0');
    let norm = if trimmed.is_empty() { "0" } else { trimmed };
    let ch = (norm.len() <= 7)
        .then(|| norm.parse::<u32>().ok())
        .flatten()
        .and_then(char::from_u32)?;
    let after_digits = &after_hash[digit_len..];
    Some((ch, after_digits.strip_prefix(';').unwrap_or(after_digits)))
}

pub(super) fn attr_is_password_type(raw: &str) -> bool {
    if raw.trim().eq_ignore_ascii_case("password") {
        return true;
    }
    if !raw.contains("&#") {
        return false;
    }
    decode_numeric_entities(raw)
        .trim()
        .eq_ignore_ascii_case("password")
}

pub(super) fn decode_attr_entities(raw: &str) -> String {
    let num = decode_numeric_entities(raw);
    if !num.contains('&') {
        return num;
    }
    let mut out = String::with_capacity(num.len());
    let mut rest = num.as_str();
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let matched = [
            ("sol;", '/'),
            ("plus;", '+'),
            ("colon;", ':'),
            ("semi;", ';'),
            ("Tab;", '\t'),
            ("NewLine;", '\n'),
            ("amp;", '&'),
            ("lt;", '<'),
            ("gt;", '>'),
            ("quot;", '"'),
            ("apos;", '\''),
        ]
        .into_iter()
        .find_map(|(ent, ch)| after.strip_prefix(ent).map(|tail| (ch, tail)));
        if let Some((ch, tail)) = matched {
            out.push(ch);
            rest = tail;
        } else {
            out.push('&');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}
