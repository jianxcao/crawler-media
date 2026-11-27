use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;

use crate::http::err;
use plugin_package::ArchiveLimits;

/// Route-level body limit (66 MiB) overriding the router's global 20 MiB cap
/// so up to 64 MiB `.cmplugin` archives plus multipart framing & `.minisig` fit.
pub(super) const MAX_UPLOAD_BODY_BYTES: usize = 66 * 1024 * 1024;
const MAX_SIGNATURE_BYTES: usize = 64 * 1024;
const MAX_SMALL_FIELD_BYTES: usize = 8 * 1024;

pub(super) struct ParsedUploadMultipart {
    pub upload_id: Option<String>,
    pub file: Vec<u8>,
    pub signature: Option<String>,
    pub expected_sha256: Option<String>,
    pub reauth_token: Option<String>,
}

pub(super) fn parse_upload_multipart(
    headers: &HeaderMap,
    body: &[u8],
) -> Result<ParsedUploadMultipart, Response> {
    let boundary = extract_boundary(headers)?;
    let parts = split_multipart_parts(body, &boundary)?;
    let max_pkg = ArchiveLimits::default().max_package_bytes as usize;

    let mut upload_id: Option<String> = None;
    let mut file: Option<Vec<u8>> = None;
    let mut signature: Option<String> = None;
    let mut expected_sha256: Option<String> = None;
    let mut reauth_token: Option<String> = None;

    for (part_headers, data) in parts {
        let name = parse_part_name(part_headers)?;
        match name.as_str() {
            "upload_id" => {
                set_text_field(&mut upload_id, "upload_id", data, MAX_SMALL_FIELD_BYTES)?
            }
            "reauth_token" => {
                set_text_field(
                    &mut reauth_token,
                    "reauth_token",
                    data,
                    MAX_SMALL_FIELD_BYTES,
                )?;
            }
            "expected_sha256" => {
                set_text_field(
                    &mut expected_sha256,
                    "expected_sha256",
                    data,
                    MAX_SMALL_FIELD_BYTES,
                )?;
            }
            "signature" => {
                if signature.is_some() {
                    return Err(err(
                        StatusCode::BAD_REQUEST,
                        "request.invalid",
                        "duplicate signature field",
                    ));
                }
                if data.len() > MAX_SIGNATURE_BYTES {
                    return Err(err(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "plugin.signature_too_large",
                        "signature exceeds 64 KiB limit",
                    ));
                }
                let text = std::str::from_utf8(data).map_err(|_| {
                    err(
                        StatusCode::BAD_REQUEST,
                        "plugin.invalid_package",
                        "signature must be valid UTF-8",
                    )
                })?;
                signature = Some(text.to_string());
            }
            "file" => {
                if file.is_some() {
                    return Err(err(
                        StatusCode::BAD_REQUEST,
                        "request.invalid",
                        "duplicate file field",
                    ));
                }
                if data.len() > max_pkg {
                    return Err(err(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "plugin.package_too_large",
                        "package exceeds 64 MiB limit",
                    ));
                }
                file = Some(data.to_vec());
            }
            _ => {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "request.invalid",
                    "unknown multipart field",
                ));
            }
        }
    }

    let file = file.ok_or_else(|| {
        err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "multipart field 'file' is required",
        )
    })?;

    Ok(ParsedUploadMultipart {
        upload_id,
        file,
        signature,
        expected_sha256,
        reauth_token,
    })
}

fn set_text_field(
    slot: &mut Option<String>,
    name: &str,
    data: &[u8],
    max_len: usize,
) -> Result<(), Response> {
    if slot.is_some() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            &format!("duplicate {name} field"),
        ));
    }
    if data.len() > max_len {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            &format!("{name} field exceeds maximum length"),
        ));
    }
    let text = std::str::from_utf8(data).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            &format!("{name} field must be UTF-8"),
        )
    })?;
    *slot = Some(text.to_string());
    Ok(())
}

fn extract_boundary(headers: &HeaderMap) -> Result<String, Response> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                "request.invalid",
                "missing Content-Type",
            )
        })?;
    let mut is_multipart = false;
    let mut boundary: Option<String> = None;
    for segment in split_header_segments(ct)? {
        let seg = segment.trim();
        if seg.eq_ignore_ascii_case("multipart/form-data") {
            is_multipart = true;
            continue;
        }
        if let Some((k, v)) = seg.split_once('=')
            && k.trim().eq_ignore_ascii_case("boundary")
        {
            let val = unquote_param_value(v)?;
            if val.is_empty() || val.len() > 70 {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "request.invalid",
                    "invalid multipart boundary",
                ));
            }
            boundary = Some(val);
        }
    }
    if !is_multipart {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            "Content-Type must be multipart/form-data",
        ));
    }
    boundary.ok_or_else(|| {
        err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            "missing multipart boundary",
        )
    })
}

enum DelimiterEnd {
    Close,
    NextPart(usize),
}

fn match_delimiter_end(body: &[u8], after_delim: usize) -> Option<DelimiterEnd> {
    if body.get(after_delim..after_delim + 2) == Some(b"--") {
        return Some(DelimiterEnd::Close);
    }
    let mut i = after_delim;
    while matches!(body.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    if body.get(i..i + 2) == Some(b"\r\n") {
        return Some(DelimiterEnd::NextPart(i + 2));
    }
    None
}

fn find_next_boundary(
    body: &[u8],
    start: usize,
    line_delim_bytes: &[u8],
) -> Option<(usize, DelimiterEnd)> {
    let mut search_from = start;
    while let Some(rel) = find_subslice(&body[search_from..], line_delim_bytes) {
        let match_start = search_from + rel;
        let after = match_start + line_delim_bytes.len();
        if let Some(end) = match_delimiter_end(body, after) {
            return Some((match_start, end));
        }
        search_from = match_start + 1;
    }
    None
}

fn split_multipart_parts<'a>(
    body: &'a [u8],
    boundary: &str,
) -> Result<Vec<(&'a [u8], &'a [u8])>, Response> {
    let delim = format!("--{boundary}");
    let delim_bytes = delim.as_bytes();
    let first = find_subslice(body, delim_bytes).ok_or_else(|| {
        err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            "malformed multipart body",
        )
    })?;
    let mut state = match_delimiter_end(body, first + delim_bytes.len()).ok_or_else(|| {
        err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            "malformed multipart boundary delimiter",
        )
    })?;
    let mut parts = Vec::new();
    let line_delim = format!("\r\n--{boundary}");
    let line_delim_bytes = line_delim.as_bytes();

    while let DelimiterEnd::NextPart(pos) = state {
        let header_end = find_subslice(&body[pos..], b"\r\n\r\n").ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                "request.invalid",
                "malformed multipart part headers",
            )
        })?;
        let part_headers = &body[pos..pos + header_end];
        let data_start = pos + header_end + 4;
        let (data_end, next_state) = find_next_boundary(body, data_start, line_delim_bytes)
            .ok_or_else(|| {
                err(
                    StatusCode::BAD_REQUEST,
                    "request.invalid",
                    "unterminated multipart part",
                )
            })?;
        parts.push((part_headers, &body[data_start..data_end]));
        state = next_state;
    }
    Ok(parts)
}

fn parse_part_name(header_bytes: &[u8]) -> Result<String, Response> {
    let raw = std::str::from_utf8(header_bytes).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            "non-UTF8 part headers",
        )
    })?;
    for line in raw.split("\r\n") {
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-disposition")
        {
            for param in split_header_segments(v)? {
                let param = param.trim();
                if let Some((pk, pv)) = param.split_once('=')
                    && pk.trim().eq_ignore_ascii_case("name")
                {
                    let name = unquote_param_value(pv)?;
                    if !name.is_empty() {
                        return Ok(name);
                    }
                }
            }
        }
    }
    Err(err(
        StatusCode::BAD_REQUEST,
        "request.invalid",
        "multipart part missing Content-Disposition name",
    ))
}

fn split_header_segments(input: &str) -> Result<Vec<&str>, Response> {
    let mut segments = Vec::new();
    let bytes = input.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut in_quotes = false;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                in_quotes = !in_quotes;
                i += 1;
            }
            b'\\' if in_quotes => {
                if i + 1 >= bytes.len() {
                    return Err(err(
                        StatusCode::BAD_REQUEST,
                        "request.invalid",
                        "malformed quoted header parameter",
                    ));
                }
                i += 2;
            }
            b';' if !in_quotes => {
                segments.push(&input[start..i]);
                i += 1;
                start = i;
            }
            _ => {
                i += 1;
            }
        }
    }
    if in_quotes {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            "unbalanced quote in header parameter",
        ));
    }
    segments.push(&input[start..]);
    Ok(segments)
}

fn unquote_param_value(raw: &str) -> Result<String, Response> {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix('"') {
        let Some(inner) = rest.strip_suffix('"') else {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "request.invalid",
                "unbalanced quote in header parameter",
            ));
        };
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                let Some(escaped) = chars.next() else {
                    return Err(err(
                        StatusCode::BAD_REQUEST,
                        "request.invalid",
                        "dangling escape in quoted header parameter",
                    ));
                };
                out.push(escaped);
            } else if c == '"' {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "request.invalid",
                    "unexpected unescaped quote in header parameter",
                ));
            } else {
                out.push(c);
            }
        }
        return Ok(out);
    }
    if trimmed.contains('"') {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            "unbalanced quote in header parameter",
        ));
    }
    Ok(trimmed.to_string())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ct_headers(val: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, val.parse().unwrap());
        h
    }

    #[test]
    fn quote_aware_boundary_and_part_name_parsing() {
        let b =
            extract_boundary(&ct_headers(r#"multipart/form-data; boundary="abc;def""#)).unwrap();
        assert_eq!(b, "abc;def");
        assert!(extract_boundary(&ct_headers(r#"multipart/form-data; boundary="UnbalB"#)).is_err());
        assert!(extract_boundary(&ct_headers(r#"multipart/form-data; boundary=""#)).is_err());

        let disp =
            br#"Content-Disposition: form-data; filename="a; name=evil.cmplugin"; name="file""#;
        assert_eq!(parse_part_name(disp).unwrap(), "file");
        let esc = br#"Content-Disposition: form-data; filename="a;\"b.cmplugin"; name="file""#;
        assert_eq!(parse_part_name(esc).unwrap(), "file");
        let unbal = br#"Content-Disposition: form-data; filename="a; name="file""#;
        assert!(parse_part_name(unbal).is_err());
    }

    #[test]
    fn binary_payload_with_boundary_prefix_and_lwsp_delimiter_passes() {
        let body = b"--B1   \r\nContent-Disposition: form-data; name=\"file\"\r\n\r\nhello\r\n--B1EXTRA\r\nworld\r\n--B1--\r\n";
        let parts = split_multipart_parts(body, "B1").unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].1, b"hello\r\n--B1EXTRA\r\nworld");

        let bad_first =
            b"--B1_extra\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\nhi\r\n--B1--\r\n";
        assert!(split_multipart_parts(bad_first, "B1").is_err());
    }
}
