use crate::bindings::crawler::plugin::{http, site, types};
use crate::ports::{State, api_error};
use plugin_api::{self as api, ErrorCode, GuestRole, HttpTarget};

pub(crate) const FORBIDDEN_REQUEST_HEADERS: &[&str] = &[
    "host",
    "cookie",
    "set-cookie",
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "connection",
    "content-length",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

const SENSITIVE_RESPONSE_HEADERS: &[&str] = &[
    "set-cookie",
    "cookie",
    "authorization",
    "proxy-authorization",
    "x-api-key",
];

pub(crate) fn sanitize_http_response(mut resp: api::HttpResponse) -> api::HttpResponse {
    resp.headers.retain(|h| {
        let lower = h.name.trim().to_ascii_lowercase();
        !SENSITIVE_RESPONSE_HEADERS.contains(&lower.as_str())
    });
    resp
}

pub(crate) fn strip_site_prefix(handle: &str) -> &str {
    let trimmed = handle.trim();
    if trimmed
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("site:"))
    {
        trimmed[5..].trim()
    } else {
        trimmed
    }
}

pub(crate) fn has_path_traversal(raw_path: &str) -> bool {
    let lower = raw_path.to_ascii_lowercase();
    lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || lower.split('/').any(|seg| {
            let core = seg.split(';').next().unwrap_or(seg);
            let core = core.split("%3b").next().unwrap_or(core);
            core == ".." || (core == "." && core != seg)
        })
}

pub(crate) fn check_site_relative_path(path: &str) -> Result<(), &'static str> {
    if path.chars().any(|c| c.is_control() || c.is_whitespace())
        || path.contains('\\')
        || path.contains('?')
        || path.contains('#')
        || path.contains("//")
    {
        return Err("invalid site-relative path");
    }
    let lower = path.to_ascii_lowercase();
    let prefix = lower.split(['/', '?', '#']).next().unwrap_or("");
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.contains("://")
        || prefix.contains(':')
    {
        return Err("site-relative path cannot specify scheme");
    }
    if has_path_traversal(path) {
        return Err("site-relative path contains traversal segments");
    }
    Ok(())
}

pub(crate) fn canonical_site_id(raw: Option<&str>) -> Option<&str> {
    raw.map(strip_site_prefix).filter(|s| !s.is_empty())
}

fn validate_http_request(state: &State, req: &api::HttpRequest) -> Result<(), types::ApiError> {
    if state.invocation.role == GuestRole::Lifecycle {
        return Err(api_error(
            types::ErrorCode::WrongContext,
            "http is forbidden during lifecycle validation",
        ));
    }
    let method = req.method.trim().to_ascii_uppercase();
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Err(api_error(
            types::ErrorCode::InvalidArgument,
            "invalid http method",
        ));
    }
    if matches!(state.invocation.role, GuestRole::Filter | GuestRole::Gate) && method != "GET" {
        return Err(api_error(
            types::ErrorCode::WrongContext,
            "filter and gate hooks only allow GET requests",
        ));
    }
    if req.max_bytes == 0 {
        return Err(api_error(
            types::ErrorCode::InvalidArgument,
            "http max_bytes must be > 0",
        ));
    }
    for header in &req.headers {
        let lower = header.name.trim().to_ascii_lowercase();
        if lower.is_empty()
            || header
                .name
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
            || header.value.chars().any(char::is_control)
            || FORBIDDEN_REQUEST_HEADERS.contains(&lower.as_str())
        {
            return Err(api_error(
                types::ErrorCode::PermissionDenied,
                "forbidden request header",
            ));
        }
    }
    if let HttpTarget::SiteRelative(target) = &req.target {
        if let Err(msg) = check_site_relative_path(&target.path) {
            return Err(api_error(types::ErrorCode::InvalidArgument, msg));
        }
        let Some(site_id) = canonical_site_id(state.invocation.site_id.as_deref()) else {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                "site-relative http target requires invocation site_id",
            ));
        };
        if let Some(slot) = target.auth_slot.as_deref() {
            state.check("http:site-auth")?;
            if state
                .invocation
                .credential_epoch
                .as_deref()
                .is_none_or(|ep| ep.trim().is_empty())
            {
                return Err(api_error(
                    types::ErrorCode::WrongContext,
                    "http:site-auth requires bound credential_epoch",
                ));
            }
            let stripped = strip_site_prefix(slot);
            if stripped.is_empty() {
                return Err(api_error(
                    types::ErrorCode::InvalidArgument,
                    "auth_slot must not be empty",
                ));
            }
            if slot
                .trim()
                .get(..5)
                .is_some_and(|p| p.eq_ignore_ascii_case("site:"))
                && stripped != site_id
            {
                return Err(api_error(
                    types::ErrorCode::PermissionDenied,
                    "auth_slot cannot reference another site",
                ));
            }
        }
    }
    Ok(())
}

fn check_site_access(state: &State, handle: &str) -> Result<(), types::ApiError> {
    if state.invocation.role == GuestRole::Lifecycle {
        return Err(api_error(
            types::ErrorCode::WrongContext,
            "site port is forbidden during lifecycle validation",
        ));
    }
    state.check("read:sites")?;
    let normalized = strip_site_prefix(handle);
    if normalized.trim().is_empty() {
        return Err(api_error(
            types::ErrorCode::InvalidArgument,
            "site handle must not be empty",
        ));
    }
    if let Some(bound) = canonical_site_id(state.invocation.site_id.as_deref())
        && normalized != bound
    {
        return Err(api_error(
            types::ErrorCode::PermissionDenied,
            "site access scoped to invocation site",
        ));
    }
    Ok(())
}

impl http::Host for State {
    fn fetch(
        &mut self,
        request: types::HttpRequest,
    ) -> Result<types::HttpResponse, types::ApiError> {
        if self.invocation.role == GuestRole::Lifecycle {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                "http is forbidden during lifecycle validation",
            ));
        }
        self.check("http")?;
        let api_req: api::HttpRequest = request.into();
        validate_http_request(self, &api_req)?;
        if self
            .http_calls_used
            .checked_add(1)
            .is_none_or(|used| used > self.invocation.max_http_calls)
        {
            return Err(api_error(
                types::ErrorCode::QuotaExceeded,
                "http call quota exceeded",
            ));
        }
        self.http_calls_used += 1;
        let resp = self
            .ports
            .http_fetch(&self.invocation, api_req)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(sanitize_http_response(resp).into())
    }

    fn fetch_many(
        &mut self,
        requests: Vec<types::HttpRequest>,
    ) -> Vec<Result<types::HttpResponse, types::ApiError>> {
        if requests.is_empty() {
            return vec![Err(api_error(
                types::ErrorCode::InvalidArgument,
                "fetch-many requires 1..=8 requests",
            ))];
        }
        let count = requests.len();
        if count > 8 {
            return vec![
                Err(api_error(
                    types::ErrorCode::InvalidArgument,
                    "fetch-many allows at most 8 requests",
                ));
                count
            ];
        }
        if self.invocation.role == GuestRole::Lifecycle {
            return vec![
                Err(api_error(
                    types::ErrorCode::WrongContext,
                    "http is forbidden during lifecycle validation",
                ));
                count
            ];
        }
        if let Err(err) = self.check("http") {
            return vec![Err(err); count];
        }
        let needed = count as u32;
        if self
            .http_calls_used
            .checked_add(needed)
            .is_none_or(|used| used > self.invocation.max_http_calls)
        {
            return vec![
                Err(api_error(
                    types::ErrorCode::QuotaExceeded,
                    "http call quota exceeded",
                ));
                count
            ];
        }
        self.http_calls_used += needed;

        let mut results: Vec<Option<Result<types::HttpResponse, types::ApiError>>> =
            vec![None; count];
        let mut valid_indices = Vec::with_capacity(count);
        let mut valid_reqs = Vec::with_capacity(count);
        for (idx, req) in requests.into_iter().enumerate() {
            let api_req: api::HttpRequest = req.into();
            if let Err(err) = validate_http_request(self, &api_req) {
                results[idx] = Some(Err(err));
            } else {
                valid_indices.push(idx);
                valid_reqs.push(api_req);
            }
        }
        if !valid_reqs.is_empty() {
            let fetched = self.ports.http_fetch_many(&self.invocation, valid_reqs);
            for (idx, res) in valid_indices.into_iter().zip(fetched) {
                let mapped = match res {
                    Ok(resp) => Ok(sanitize_http_response(resp).into()),
                    Err(err) => Err(err.into()),
                };
                results[idx] = Some(mapped);
            }
        }
        let alive_err = self.check_alive().err();
        results
            .into_iter()
            .map(|item| {
                if let Some(ref err) = alive_err {
                    return Err(err.clone());
                }
                item.unwrap_or_else(|| {
                    Err(api_error(
                        types::ErrorCode::from(ErrorCode::UnknownOutcome),
                        "missing http_fetch_many response",
                    ))
                })
            })
            .collect()
    }
}

impl site::Host for State {
    fn list(&mut self, page: types::PageRequest) -> Result<types::SitePage, types::ApiError> {
        if self.invocation.role == GuestRole::Lifecycle {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                "site port is forbidden during lifecycle validation",
            ));
        }
        self.check("read:sites")?;
        let mut out = self
            .ports
            .site_list(&self.invocation, page.into())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        if let Some(bound) = canonical_site_id(self.invocation.site_id.as_deref()) {
            out.items.retain(|item| {
                strip_site_prefix(&item.id) == bound || strip_site_prefix(&item.handle) == bound
            });
            out.next_cursor = None;
        }
        Ok(out.into())
    }

    fn get(&mut self, handle: String) -> Result<types::SiteView, types::ApiError> {
        check_site_access(self, &handle)?;
        let out = self
            .ports
            .site_get(&self.invocation, &handle)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }

    fn stats(&mut self, handle: String) -> Result<types::StatsView, types::ApiError> {
        check_site_access(self, &handle)?;
        let out = self
            .ports
            .site_stats(&self.invocation, &handle)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::Invocation;
    use crate::limits::MemoryBudget;
    use crate::ports::HostPorts;
    use plugin_api::{HttpTarget, SiteTarget};

    #[derive(Default)]
    struct DummyPorts;
    impl HostPorts for DummyPorts {
        fn authorized(&self, _: &Invocation, cap: &str) -> bool {
            matches!(cap, "http" | "http:site-auth")
        }
    }

    fn make_state(site_id: &str) -> State {
        let mut inv = Invocation::new("org.example.p", Duration::from_secs(5));
        inv.role = GuestRole::Indexer;
        inv.site_id = Some(site_id.into());
        inv.credential_epoch = Some("ep1".into());
        let context = inv.context(0);
        State {
            invocation: inv,
            context,
            ports: Arc::new(DummyPorts),
            memory: MemoryBudget::new(1024 * 1024),
            http_calls_used: 0,
            browsers: HashMap::new(),
            next_browser_rep: 1,
        }
    }

    #[test]
    fn validate_http_request_trims_site_prefix_for_auth_slot() {
        let state = make_state("site-a");
        let make_req = |slot: &str| api::HttpRequest {
            method: "GET".into(),
            target: HttpTarget::SiteRelative(SiteTarget {
                path: "/".into(),
                query: vec![],
                auth_slot: Some(slot.into()),
            }),
            headers: vec![],
            body: None,
            max_bytes: 1024,
        };
        assert!(validate_http_request(&state, &make_req("site: site-a")).is_ok());
        assert!(validate_http_request(&state, &make_req("SITE:  site-a  ")).is_ok());
        let empty = validate_http_request(&state, &make_req("site:   ")).unwrap_err();
        assert_eq!(empty.code, types::ErrorCode::InvalidArgument);
        let other = validate_http_request(&state, &make_req("site: site-b")).unwrap_err();
        assert_eq!(other.code, types::ErrorCode::PermissionDenied);
    }
}
