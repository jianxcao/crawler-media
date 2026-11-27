use crate::bindings::crawler::plugin::types;
use crate::bindings::indexer_world::crawler::plugin::browser;
use crate::http_site_ports::sanitize_http_response;
use crate::ports::{State, api_error};
use plugin_api::{
    ApiError, BrowserElement, BrowserFillValue, BrowserPurpose, GuestRole, HttpResponse,
    PluginTrust,
};
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use wasmtime::component::Resource;

const MAX_BROWSER_BYTES: u32 = 5 * 1024 * 1024;
const MAX_QUERY_LIMIT: u32 = 200;
const ALLOWED_ELEMENT_ATTRIBUTES: &[&str] = &[
    "href",
    "src",
    "value",
    "text",
    "title",
    "alt",
    "class",
    "id",
    "name",
    "type",
    "placeholder",
    "rel",
    "target",
    "role",
    "tabindex",
    "disabled",
    "checked",
    "selected",
    "readonly",
    "required",
    "action",
    "method",
    "for",
    "lang",
    "dir",
    "width",
    "height",
    "colspan",
    "rowspan",
];

fn validate_site_relative_nav_url(raw: &str) -> Result<(), types::ApiError> {
    if raw.is_empty()
        || raw.chars().any(|c| c.is_control() || c.is_whitespace())
        || raw.contains('\\')
    {
        return Err(api_error(
            types::ErrorCode::InvalidArgument,
            "site_relative_url must be non-empty and contain no whitespace or control characters",
        ));
    }
    let lower = raw.to_ascii_lowercase();
    let raw_path = raw.split(['?', '#']).next().unwrap_or("");
    let lower_path = raw_path.to_ascii_lowercase();
    let prefix = lower.split(['/', '?', '#']).next().unwrap_or("");
    if lower.starts_with("//")
        || lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("javascript:")
        || lower.starts_with("data:")
        || lower.starts_with("file:")
        || lower_path.contains("://")
        || prefix.contains(':')
    {
        return Err(types::ApiError {
            code: types::ErrorCode::PermissionDenied,
            message: "external or non-relative URL blocked".into(),
            detail: Some(types::ErrorDetail::BlockedAddress),
            retry_after_ms: None,
            operation_id: None,
        });
    }
    if raw_path.contains("//") || crate::http_site_ports::has_path_traversal(raw_path) {
        return Err(api_error(
            types::ErrorCode::InvalidArgument,
            "site_relative_url contains forbidden traversal or empty segments",
        ));
    }
    Ok(())
}

fn validate_attribute_name(name: &str) -> Result<(), types::ApiError> {
    if name.is_empty() || name.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(api_error(
            types::ErrorCode::InvalidArgument,
            "attribute name must not be empty or contain whitespace",
        ));
    }
    let lower = name.to_ascii_lowercase();
    let valid_chars = lower
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let is_data_or_aria = (lower.starts_with("data-") && lower.len() > 5)
        || (lower.starts_with("aria-") && lower.len() > 5);
    if !valid_chars
        || lower.starts_with("on")
        || (!ALLOWED_ELEMENT_ATTRIBUTES.contains(&lower.as_str()) && !is_data_or_aria)
    {
        return Err(api_error(
            types::ErrorCode::PermissionDenied,
            "attribute access denied",
        ));
    }
    Ok(())
}

pub trait BrowserSessionPort: Send + Sync {
    fn navigate(&mut self, site_relative_url: &str, wait_until: &str) -> Result<String, ApiError>;
    fn dom(&mut self, max_bytes: u32) -> Result<String, ApiError>;
    fn query(&mut self, selector: &str, limit: u32) -> Result<Vec<BrowserElement>, ApiError>;
    fn text(&mut self, element: &str, max_bytes: u32) -> Result<String, ApiError>;
    fn attribute(&mut self, element: &str, name: &str) -> Result<Option<String>, ApiError>;
    fn click(&mut self, element: &str) -> Result<String, ApiError>;
    fn fill(&mut self, element: &str, value: BrowserFillValue) -> Result<(), ApiError>;
    fn wait_for(
        &mut self,
        selector: Option<&str>,
        condition: &str,
        timeout_ms: u64,
    ) -> Result<String, ApiError>;
    fn read_response(&mut self, receipt: &str, max_bytes: u32) -> Result<HttpResponse, ApiError>;
    fn capture_session(&mut self) -> Result<String, ApiError>;
    fn close(&mut self) -> Result<(), ApiError>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionState {
    Open,
    Closed,
    Quarantined,
}

pub(crate) struct ManagedBrowserSession {
    session_id: u64,
    purpose: BrowserPurpose,
    doc_epoch: u64,
    next_elem: u64,
    issued_handles: HashMap<String, (u64, String)>,
    issued_receipts: HashSet<String>,
    state: SessionState,
    inner: Option<Box<dyn BrowserSessionPort>>,
}

pub(crate) fn close_all_browsers(browsers: &mut HashMap<u32, ManagedBrowserSession>) {
    for (_, mut session) in browsers.drain() {
        if session.state == SessionState::Open
            && let Some(mut inner) = session.inner.take()
        {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.close())) {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::warn!(error = %err, session_id = session.session_id, "browser session close failed during state drop");
                }
                Err(_) => {
                    tracing::error!(
                        session_id = session.session_id,
                        "browser session close panicked during state drop"
                    );
                }
            }
        }
    }
}

fn validate_max_bytes(max_bytes: u32) -> Result<(), types::ApiError> {
    if max_bytes == 0 || max_bytes > MAX_BROWSER_BYTES {
        return Err(api_error(
            types::ErrorCode::InvalidArgument,
            "max_bytes must be within 1..=5MiB",
        ));
    }
    Ok(())
}

fn open_session_mut(
    state: &mut State,
    rep: u32,
) -> Result<&mut ManagedBrowserSession, types::ApiError> {
    let Some(session) = state.browsers.get_mut(&rep) else {
        return Err(api_error(
            types::ErrorCode::StaleHandle,
            "unknown browser session",
        ));
    };
    if session.state != SessionState::Open || session.inner.is_none() {
        return Err(api_error(
            types::ErrorCode::StaleHandle,
            "browser session is closed or quarantined",
        ));
    }
    Ok(session)
}

fn resolve_element(
    session: &ManagedBrowserSession,
    opaque: &str,
) -> Result<String, types::ApiError> {
    let Some((epoch, raw)) = session.issued_handles.get(opaque) else {
        return Err(api_error(
            types::ErrorCode::StaleHandle,
            "stale or unknown element handle",
        ));
    };
    if *epoch != session.doc_epoch {
        return Err(api_error(
            types::ErrorCode::StaleHandle,
            "element handle expired across navigation",
        ));
    }
    Ok(raw.clone())
}

impl browser::Host for State {
    fn open(
        &mut self,
        purpose: browser::Purpose,
    ) -> Result<Resource<browser::Session>, types::ApiError> {
        self.check_alive()?;
        let api_purpose: BrowserPurpose = purpose.into();
        let role_ok = matches!(
            (self.invocation.role, api_purpose),
            (GuestRole::Indexer, BrowserPurpose::Search)
                | (GuestRole::Maintenance, BrowserPurpose::Maintenance)
        );
        if !role_ok {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                "browser purpose does not match invocation role",
            ));
        }
        if crate::http_site_ports::canonical_site_id(self.invocation.site_id.as_deref()).is_none() {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                "browser.open requires bound site_id",
            ));
        }
        if self.invocation.trust == PluginTrust::Untrusted {
            return Err(types::ApiError {
                code: types::ErrorCode::Unsupported,
                message: "browser isolation unavailable for untrusted plugins".into(),
                detail: Some(types::ErrorDetail::BrowserIsolationUnavailable),
                retry_after_ms: None,
                operation_id: None,
            });
        }
        let cap = match api_purpose {
            BrowserPurpose::Search => "browser:search",
            BrowserPurpose::Maintenance => "browser:maintenance",
        };
        self.check(cap)?;
        let port = self
            .ports
            .browser_open(&self.invocation, api_purpose)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;

        let mut rep = self.next_browser_rep.max(1);
        while self.browsers.contains_key(&rep) {
            rep = rep.wrapping_add(1).max(1);
        }
        self.next_browser_rep = rep.wrapping_add(1).max(1);
        self.browsers.insert(
            rep,
            ManagedBrowserSession {
                session_id: u64::from(rep),
                purpose: api_purpose,
                doc_epoch: 1,
                next_elem: 1,
                issued_handles: HashMap::new(),
                issued_receipts: HashSet::new(),
                state: SessionState::Open,
                inner: Some(port),
            },
        );
        Ok(Resource::new_own(rep))
    }
}

impl browser::HostSession for State {
    fn navigate(
        &mut self,
        self_: Resource<browser::Session>,
        site_relative_url: String,
        wait_until: String,
    ) -> Result<String, types::ApiError> {
        self.check_alive()?;
        let session = open_session_mut(self, self_.rep())?;
        validate_site_relative_nav_url(&site_relative_url)?;
        session.doc_epoch += 1;
        session.issued_handles.clear();
        let inner = session
            .inner
            .as_mut()
            .ok_or_else(|| api_error(types::ErrorCode::StaleHandle, "session closed"))?;
        let receipt = inner
            .navigate(&site_relative_url, &wait_until)
            .map_err(types::ApiError::from)?;
        session.issued_receipts.insert(receipt.clone());
        self.check_alive()?;
        Ok(receipt)
    }

    fn dom(
        &mut self,
        self_: Resource<browser::Session>,
        max_bytes: u32,
    ) -> Result<String, types::ApiError> {
        self.check_alive()?;
        validate_max_bytes(max_bytes)?;
        let session = open_session_mut(self, self_.rep())?;
        let inner = session
            .inner
            .as_mut()
            .ok_or_else(|| api_error(types::ErrorCode::StaleHandle, "session closed"))?;
        let out = inner.dom(max_bytes).map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out)
    }

    fn query(
        &mut self,
        self_: Resource<browser::Session>,
        selector: String,
        limit: u32,
    ) -> Result<Vec<browser::Element>, types::ApiError> {
        self.check_alive()?;
        if selector.trim().is_empty() || limit == 0 || limit > MAX_QUERY_LIMIT {
            return Err(api_error(
                types::ErrorCode::InvalidArgument,
                "invalid selector or query limit",
            ));
        }
        let session = open_session_mut(self, self_.rep())?;
        let inner = session
            .inner
            .as_mut()
            .ok_or_else(|| api_error(types::ErrorCode::StaleHandle, "session closed"))?;
        let raw_items = inner
            .query(&selector, limit)
            .map_err(types::ApiError::from)?;
        let mut out = Vec::with_capacity(raw_items.len().min(limit as usize));
        for item in raw_items.into_iter().take(limit as usize) {
            let seq = session.next_elem;
            session.next_elem += 1;
            let opaque = format!("el-{}-{}-{}", session.session_id, session.doc_epoch, seq);
            session
                .issued_handles
                .insert(opaque.clone(), (session.doc_epoch, item.handle));
            out.push(browser::Element {
                handle: opaque,
                visible_text: item.visible_text,
            });
        }
        self.check_alive()?;
        Ok(out)
    }

    fn text(
        &mut self,
        self_: Resource<browser::Session>,
        element: String,
        max_bytes: u32,
    ) -> Result<String, types::ApiError> {
        self.check_alive()?;
        validate_max_bytes(max_bytes)?;
        let session = open_session_mut(self, self_.rep())?;
        let raw = resolve_element(session, &element)?;
        let inner = session
            .inner
            .as_mut()
            .ok_or_else(|| api_error(types::ErrorCode::StaleHandle, "session closed"))?;
        let out = inner.text(&raw, max_bytes).map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out)
    }

    fn attribute(
        &mut self,
        self_: Resource<browser::Session>,
        element: String,
        name: String,
    ) -> Result<Option<String>, types::ApiError> {
        self.check_alive()?;
        validate_attribute_name(&name)?;
        let session = open_session_mut(self, self_.rep())?;
        let raw = resolve_element(session, &element)?;
        let inner = session
            .inner
            .as_mut()
            .ok_or_else(|| api_error(types::ErrorCode::StaleHandle, "session closed"))?;
        let out = inner
            .attribute(&raw, &name)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out)
    }

    fn click(
        &mut self,
        self_: Resource<browser::Session>,
        element: String,
    ) -> Result<String, types::ApiError> {
        self.check_alive()?;
        let session = open_session_mut(self, self_.rep())?;
        let raw = resolve_element(session, &element)?;
        session.doc_epoch += 1;
        session.issued_handles.clear();
        let inner = session
            .inner
            .as_mut()
            .ok_or_else(|| api_error(types::ErrorCode::StaleHandle, "session closed"))?;
        let out = inner.click(&raw).map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out)
    }

    fn fill(
        &mut self,
        self_: Resource<browser::Session>,
        element: String,
        value: browser::FillValue,
    ) -> Result<(), types::ApiError> {
        self.check_alive()?;
        if let browser::FillValue::CredentialSlot(ref slot) = value {
            let bound_site =
                crate::http_site_ports::canonical_site_id(self.invocation.site_id.as_deref())
                    .ok_or_else(|| {
                        api_error(
                            types::ErrorCode::WrongContext,
                            "browser.fill credential-slot requires bound site_id",
                        )
                    })?;
            let stripped = crate::http_site_ports::strip_site_prefix(slot);
            if stripped.is_empty() {
                return Err(api_error(
                    types::ErrorCode::InvalidArgument,
                    "credential_slot must not be empty",
                ));
            }
            if slot
                .trim()
                .get(..5)
                .is_some_and(|p| p.eq_ignore_ascii_case("site:"))
                && stripped != bound_site
            {
                return Err(api_error(
                    types::ErrorCode::PermissionDenied,
                    "credential_slot cannot reference another site",
                ));
            }
        }
        let session = open_session_mut(self, self_.rep())?;
        let raw = resolve_element(session, &element)?;
        let inner = session
            .inner
            .as_mut()
            .ok_or_else(|| api_error(types::ErrorCode::StaleHandle, "session closed"))?;
        inner
            .fill(&raw, value.into())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(())
    }

    fn wait_for(
        &mut self,
        self_: Resource<browser::Session>,
        selector: Option<String>,
        condition: String,
        timeout_ms: u64,
    ) -> Result<String, types::ApiError> {
        self.check_alive()?;
        let remaining_ms = self
            .invocation
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        let clamped_timeout = timeout_ms.min(remaining_ms);
        let session = open_session_mut(self, self_.rep())?;
        let inner = session
            .inner
            .as_mut()
            .ok_or_else(|| api_error(types::ErrorCode::StaleHandle, "session closed"))?;
        let out = inner
            .wait_for(selector.as_deref(), &condition, clamped_timeout)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out)
    }

    fn read_response(
        &mut self,
        self_: Resource<browser::Session>,
        receipt: String,
        max_bytes: u32,
    ) -> Result<types::HttpResponse, types::ApiError> {
        self.check_alive()?;
        validate_max_bytes(max_bytes)?;
        let session = open_session_mut(self, self_.rep())?;
        if !session.issued_receipts.contains(&receipt) {
            return Err(api_error(
                types::ErrorCode::StaleHandle,
                "unknown or expired navigation receipt",
            ));
        }
        let inner = session
            .inner
            .as_mut()
            .ok_or_else(|| api_error(types::ErrorCode::StaleHandle, "session closed"))?;
        let resp = inner
            .read_response(&receipt, max_bytes)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(sanitize_http_response(resp).into())
    }

    fn capture_session(
        &mut self,
        self_: Resource<browser::Session>,
    ) -> Result<String, types::ApiError> {
        self.check_alive()?;
        if self.invocation.role != GuestRole::Maintenance {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                "capture_session is restricted to maintenance contexts",
            ));
        }
        let session = open_session_mut(self, self_.rep())?;
        if session.purpose != BrowserPurpose::Maintenance {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                "capture_session requires maintenance browser purpose",
            ));
        }
        let inner = session
            .inner
            .as_mut()
            .ok_or_else(|| api_error(types::ErrorCode::StaleHandle, "session closed"))?;
        let out = inner.capture_session().map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out)
    }

    fn close(&mut self, self_: Resource<browser::Session>) -> Result<(), types::ApiError> {
        let Some(session) = self.browsers.get_mut(&self_.rep()) else {
            return Err(api_error(
                types::ErrorCode::StaleHandle,
                "unknown browser session",
            ));
        };
        match session.state {
            SessionState::Closed => return Ok(()),
            SessionState::Quarantined => {
                return Err(api_error(
                    types::ErrorCode::Conflict,
                    "browser session quarantined after cleanup failure",
                ));
            }
            SessionState::Open => {}
        }
        let Some(inner) = session.inner.as_mut() else {
            session.state = SessionState::Closed;
            return Ok(());
        };
        match inner.close() {
            Ok(()) => {
                session.state = SessionState::Closed;
                session.inner = None;
                Ok(())
            }
            Err(err) => {
                tracing::warn!(error = %err, session_id = session.session_id, "browser session close failed; quarantining session");
                session.state = SessionState::Quarantined;
                Err(err.into())
            }
        }
    }

    fn drop(&mut self, rep: Resource<browser::Session>) -> wasmtime::Result<()> {
        if let Some(mut session) = self.browsers.remove(&rep.rep())
            && session.state == SessionState::Open
            && let Some(mut inner) = session.inner.take()
            && let Err(err) = inner.close()
        {
            tracing::warn!(error = %err, session_id = session.session_id, "browser session close failed on resource drop");
        }
        Ok(())
    }
}
