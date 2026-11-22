use std::io::Read;

use crate::http_agent;

const MAX_POSTER_BYTES: u64 = 12 * 1024 * 1024;
const MAX_POSTER_REDIRECTS: usize = 5;

pub(crate) fn allowed_fanart_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || url.split_once("://").is_some_and(|(_, rest)| {
            rest.split(['/', '\\', '?', '#'])
                .next()
                .unwrap_or("")
                .contains('@')
        })
    {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    if parsed
        .host()
        .is_some_and(|host| !matches!(host, url::Host::Domain(_)))
        || host.eq_ignore_ascii_case("localhost")
    {
        return false;
    }
    let lower = host.to_ascii_lowercase();
    if lower == "webservice.fanart.tv" && parsed.scheme() != "https" {
        return false;
    }
    lower == "fanart.tv" || lower.ends_with(".fanart.tv")
}

fn redact_url_for_error(raw: &str) -> String {
    if let Ok(mut parsed) = url::Url::parse(raw) {
        let _ = parsed.set_username("");
        let _ = parsed.set_password(None);
        parsed.set_query(None);
        parsed.set_fragment(None);
        return parsed.to_string();
    }
    let without_query = raw.split(['?', '#']).next().unwrap_or(raw);
    if let Some((scheme, rest)) = without_query.split_once("://") {
        let (authority, tail) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, ""),
        };
        if let Some((_, host_part)) = authority.rsplit_once('@') {
            return format!("{scheme}://{host_part}{tail}");
        }
    }
    without_query.to_string()
}

fn redact_error_message(url: &str, message: String) -> String {
    let redacted = redact_url_for_error(url);
    let mut out = message.replace(url, &redacted);
    if let Some((_, rest)) = url.split_once("://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        if let Some((userinfo, _)) = authority.rsplit_once('@')
            && !userinfo.is_empty()
        {
            out = out.replace(&format!("{userinfo}@"), "");
            if userinfo.contains(':') {
                out = out.replace(userinfo, "[redacted]");
            }
        }
    }
    if let Ok(parsed) = url::Url::parse(url) {
        let parsed_str = parsed.as_str();
        if parsed_str != url {
            out = out.replace(parsed_str, &redacted);
        }
        let userinfo = &parsed[url::Position::BeforeUsername..url::Position::AfterPassword];
        if !userinfo.is_empty() {
            out = out.replace(&format!("{userinfo}@"), "");
            if userinfo.contains(':') {
                out = out.replace(userinfo, "[redacted]");
            }
        }
        if let Some(query) = parsed.query()
            && !query.is_empty()
        {
            out = out.replace(&format!("?{query}"), "");
        }
        if let Some(fragment) = parsed.fragment()
            && !fragment.is_empty()
        {
            out = out.replace(&format!("#{fragment}"), "");
        }
    } else {
        if let Some((_, query)) = url.split_once('?') {
            let query = query.split('#').next().unwrap_or(query);
            if !query.is_empty() {
                out = out.replace(&format!("?{query}"), "");
            }
        }
        if let Some((_, fragment)) = url.split_once('#')
            && !fragment.is_empty()
        {
            out = out.replace(&format!("#{fragment}"), "");
        }
    }
    out
}

pub(super) fn fetch_with_policy(
    url: &str,
    allowed: impl Fn(&str) -> bool,
) -> Result<Vec<u8>, String> {
    let mut current = url.to_string();
    for _ in 0..=MAX_POSTER_REDIRECTS {
        if !allowed(&current) {
            return Err(format!(
                "disallowed poster URL: {}",
                redact_url_for_error(&current)
            ));
        }
        let parsed = url::Url::parse(&current)
            .map_err(|error| redact_error_message(&current, error.to_string()))?;
        let response = http_agent::call_url(parsed.as_str(), |agent| {
            agent
                .get(parsed.as_str())
                .config()
                .max_redirects(0)
                .build()
                .call()
        })
        .map_err(|err| redact_error_message(&current, err.to_string()))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| format!("redirect {status} missing Location"))?;
            current = parsed
                .join(location)
                .map_err(|e| redact_error_message(&current, e.to_string()))?
                .to_string();
            continue;
        }
        if !status.is_success() {
            return Err(format!("upstream returned {status}"));
        }
        let mut reader = response
            .into_body()
            .into_with_config()
            .limit(MAX_POSTER_BYTES + 1)
            .reader()
            .take(MAX_POSTER_BYTES + 1);
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|e| redact_error_message(&current, e.to_string()))?;
        if bytes.len() as u64 > MAX_POSTER_BYTES {
            return Err(format!("poster response exceeds {MAX_POSTER_BYTES} bytes"));
        }
        return Ok(bytes);
    }
    Err("too many poster redirects".into())
}
