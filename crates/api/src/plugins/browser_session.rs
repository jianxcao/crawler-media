use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use domain::SiteId;
use indexer::{PluginBrowserError, PluginBrowserErrorKind, PluginBrowserLease, PluginNavResponse};
use net_guard::Origin;
use plugin_api::{
    ApiError, BrowserElement, BrowserFillValue, BrowserPurpose, ErrorCode, ErrorDetail, Header,
    HttpResponse,
};
use plugin_host::BrowserSessionPort;

use super::super::ports::{CapturedCookieRecord, StoreHostPorts, api_err, map_store_error};
use super::{MAX_BROWSER_BYTES, SENSITIVE_RESPONSE_HEADERS, blocked_browser_addr, map_browser_err};

pub(super) struct ObscuraBrowserSession {
    pub(super) ports: Arc<StoreHostPorts>,
    pub(super) store: Arc<store::Store>,
    pub(super) authority: store::plugins::PluginStateAuthority,
    pub(super) site_id: SiteId,
    pub(super) site_url: String,
    pub(super) site_origin: Origin,
    pub(super) credential_epoch: String,
    pub(super) purpose: BrowserPurpose,
    pub(super) deadline: Instant,
    pub(super) lease: PluginBrowserLease,
    pub(super) next_seq: u64,
    pub(super) element_handles: HashMap<String, String>,
    pub(super) nav_responses: HashMap<String, PluginNavResponse>,
    pub(super) post_nav_or_click: bool,
    pub(super) has_navigated: bool,
}

impl ObscuraBrowserSession {
    fn map_op_err(&self, err: PluginBrowserError) -> ApiError {
        if self.post_nav_or_click
            && matches!(
                err.kind,
                PluginBrowserErrorKind::PostNavUnknown | PluginBrowserErrorKind::Transport
            )
        {
            return api_err(ErrorCode::UnknownOutcome, &err.message);
        }
        map_browser_err(err)
    }

    fn ensure_live_session(&self) -> Result<store::SiteObservation, ApiError> {
        self.store
            .validate_plugin_state_authority(&self.authority)
            .map_err(map_store_error)?;
        let obs = self
            .store
            .observe_site(self.site_id)
            .map_err(map_store_error)?
            .ok_or_else(|| api_err(ErrorCode::StaleGeneration, "bound site not found"))?;
        if !obs.site().enabled || obs.credential_epoch().storage_key() != self.credential_epoch {
            return Err(api_err(
                ErrorCode::StaleGeneration,
                "bound site disabled or credential epoch changed",
            ));
        }
        Ok(obs)
    }

    fn resolve_site_relative_url(&self, rel: &str) -> Result<String, ApiError> {
        if rel.is_empty()
            || rel.chars().any(|c| c.is_control() || c.is_whitespace())
            || rel.contains('\\')
            || rel.starts_with("//")
            || rel.split('/').any(|seg| seg == "..")
        {
            return Err(api_err(
                ErrorCode::InvalidArgument,
                "invalid site-relative browser URL",
            ));
        }
        let lower = rel.to_ascii_lowercase();
        let prefix = lower.split(['/', '?', '#']).next().unwrap_or("");
        if lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.contains("://")
            || prefix.contains(':')
        {
            return Err(ApiError {
                code: ErrorCode::PermissionDenied,
                message: "external browser navigation URL blocked".into(),
                detail: Some(ErrorDetail::BlockedAddress),
                retry_after_ms: None,
                operation_id: None,
            });
        }
        let base = url::Url::parse(&self.site_url)
            .map_err(|_| api_err(ErrorCode::InvalidArgument, "invalid site base URL"))?;
        let split_idx = rel.find(['?', '#']).unwrap_or(rel.len());
        let (rel_path_raw, suffix) = rel.split_at(split_idx);
        if rel_path_raw.split('/').any(|seg| seg == "..") {
            return Err(api_err(
                ErrorCode::InvalidArgument,
                "invalid site-relative browser URL",
            ));
        }
        let base_path = base.path().trim_end_matches('/');
        let rel_path = rel_path_raw.trim_start_matches('/');
        let combined_path = if rel_path.is_empty() {
            if base_path.is_empty() {
                "/".to_string()
            } else if rel_path_raw.starts_with('/') {
                format!("{base_path}/")
            } else {
                base_path.to_string()
            }
        } else {
            format!("{base_path}/{rel_path}")
        };
        let candidate = format!("{}{combined_path}{suffix}", self.site_origin);
        let norm = net_guard::normalize_url(&candidate, true).map_err(|e| match e {
            net_guard::NetGuardError::BlockedAddress(msg) => blocked_browser_addr(msg),
            _ => api_err(
                ErrorCode::InvalidArgument,
                "invalid site-relative browser URL",
            ),
        })?;
        if norm.origin != self.site_origin {
            return Err(blocked_browser_addr(
                "external browser navigation URL blocked",
            ));
        }
        Ok(norm.url.into())
    }

    fn ensure_same_origin_if_navigated(&mut self) -> Result<(), ApiError> {
        if self.has_navigated {
            self.verify_current_page_origin()?;
        }
        Ok(())
    }

    fn lookup_handle(&self, opaque: &str) -> Result<String, ApiError> {
        self.element_handles
            .get(opaque)
            .cloned()
            .ok_or_else(|| api_err(ErrorCode::StaleHandle, "stale or unknown browser element"))
    }

    fn verify_current_page_origin(&mut self) -> Result<(), ApiError> {
        let loc_val = self
            .lease
            .eval_json(
                "/* __cm_origin */ (() => String((window.location && (window.location.origin && window.location.origin !== 'null' ? window.location.origin : window.location.href)) || ''))()",
            )
            .map_err(|e| self.map_op_err(e))?;
        let raw_url = loc_val.as_str().unwrap_or("");
        let matches_origin = net_guard::normalize_url(raw_url, true)
            .is_ok_and(|norm| norm.origin == self.site_origin);
        if !matches_origin {
            return Err(api_err(
                ErrorCode::PermissionDenied,
                "browser navigated away from bound site origin",
            ));
        }
        Ok(())
    }
}

impl BrowserSessionPort for ObscuraBrowserSession {
    fn navigate(&mut self, site_relative_url: &str, wait_until: &str) -> Result<String, ApiError> {
        self.ensure_live_session()?;
        let full_url = self.resolve_site_relative_url(site_relative_url)?;
        self.post_nav_or_click = true;
        self.has_navigated = true;
        self.element_handles.clear();
        let resp = self
            .lease
            .navigate(&full_url, wait_until)
            .map_err(|e| self.map_op_err(e))?;
        self.verify_current_page_origin()?;
        let receipt = format!("bnav-{}", self.next_seq);
        self.next_seq += 1;
        self.nav_responses.insert(receipt.clone(), resp);
        Ok(receipt)
    }

    fn dom(&mut self, max_bytes: u32) -> Result<String, ApiError> {
        self.ensure_live_session()?;
        self.ensure_same_origin_if_navigated()?;
        let cap = (max_bytes as usize).clamp(1, MAX_BROWSER_BYTES);
        let val = self
            .lease
            .eval_json(
                "/* __cm_dom */ (() => { const d = document.documentElement.cloneNode(true); const loc = (n) => ((n.localName || n.tagName || '').split(':').pop() || '').toLowerCase(); Array.from(d.getElementsByTagName('*')).forEach(n => { const t = loc(n); if (t === 'script' || t === 'noscript') { n.remove(); return; } const isPwd = t === 'input' && String(n.getAttribute('type') || n.type || '').trim().toLowerCase() === 'password'; if (isPwd || n.hasAttribute('data-cm-secret')) { n.setAttribute('value', ''); if (t === 'textarea') n.textContent = ''; } }); d.querySelectorAll('input[type=\"password\"], [data-cm-secret]').forEach(n => { n.setAttribute('value', ''); if (loc(n) === 'textarea') n.textContent = ''; }); return { html: d.outerHTML, xml: /xml|xhtml/i.test(String(document.contentType || '')) }; })()",
            )
            .map_err(|e| self.map_op_err(e))?;
        let (raw, explicit_xhtml) = match val.as_str() {
            Some(s) => (s, None),
            None => (
                val.get("html").and_then(|v| v.as_str()).unwrap_or(""),
                val.get("xml").and_then(|v| v.as_bool()),
            ),
        };
        Ok(truncate_utf8(
            &super::super::browser_html::sanitize_html_with_mode(raw, explicit_xhtml),
            cap,
        ))
    }

    fn query(&mut self, selector: &str, limit: u32) -> Result<Vec<BrowserElement>, ApiError> {
        self.ensure_live_session()?;
        self.ensure_same_origin_if_navigated()?;
        if selector.trim().is_empty()
            || selector.len() > 512
            || selector.chars().any(char::is_control)
        {
            return Err(api_err(ErrorCode::InvalidArgument, "invalid CSS selector"));
        }
        let sel_json = serde_json::to_string(selector)
            .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
        let capped = limit.clamp(1, 200);
        let expr = format!(
            "/* __cm_query */ (() => {{ const loc = (n) => ((n.localName || n.tagName || '').split(':').pop() || '').toLowerCase(); const els = Array.from(document.querySelectorAll({sel_json})).slice(0, {capped}); return els.map((el, i) => {{ const tag = loc(el); const isPwd = (tag === 'input' && String(el.getAttribute('type') || el.type || '').trim().toLowerCase() === 'password') || el.hasAttribute('data-cm-secret'); const isScript = tag === 'script' || tag === 'noscript'; const hid = 'cmh-' + Date.now() + '-' + i; el.setAttribute('data-cm-handle', hid); if (isPwd || isScript) return {{ handle: hid, visible_text: '' }}; const cl = el.cloneNode(true); if (cl.getElementsByTagName) Array.from(cl.getElementsByTagName('*')).forEach(n => {{ const t = loc(n); if (t === 'script' || t === 'noscript' || n.hasAttribute('data-cm-secret') || (t === 'input' && String(n.getAttribute('type') || n.type || '').trim().toLowerCase() === 'password')) n.remove(); }}); if (cl.querySelectorAll) cl.querySelectorAll('script,noscript,[data-cm-secret],input[type=\"password\"]').forEach(n => n.remove()); return {{ handle: hid, visible_text: String(cl.innerText || cl.textContent || '').slice(0, 512) }}; }}); }})()"
        );
        let val = self
            .lease
            .eval_json(&expr)
            .map_err(|e| self.map_op_err(e))?;
        let items = val.as_array().cloned().unwrap_or_default();
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let dom_id = item["handle"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("cmh-{}", self.next_seq));
            let text = super::super::browser_html::sanitize_html(
                item["visible_text"].as_str().unwrap_or(""),
            );
            let host_handle = format!("el-{}", self.next_seq);
            self.next_seq += 1;
            self.element_handles.insert(host_handle.clone(), dom_id);
            out.push(BrowserElement {
                handle: host_handle,
                visible_text: text,
            });
        }
        Ok(out)
    }

    fn text(&mut self, element: &str, max_bytes: u32) -> Result<String, ApiError> {
        self.ensure_live_session()?;
        self.ensure_same_origin_if_navigated()?;
        let dom_id = self.lookup_handle(element)?;
        let cap = (max_bytes as usize).clamp(1, MAX_BROWSER_BYTES);
        let id_json = serde_json::to_string(&dom_id)
            .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
        let expr = format!(
            "/* __cm_text */ (() => {{ const loc = (n) => ((n.localName || n.tagName || '').split(':').pop() || '').toLowerCase(); const el = document.querySelector('[data-cm-handle=' + JSON.stringify({id_json}) + ']'); if (!el) return ''; const tag = loc(el); if (tag === 'script' || tag === 'noscript' || el.hasAttribute('data-cm-secret') || (tag === 'input' && String(el.getAttribute('type') || el.type || '').trim().toLowerCase() === 'password')) return ''; const cl = el.cloneNode(true); if (cl.getElementsByTagName) Array.from(cl.getElementsByTagName('*')).forEach(n => {{ const t = loc(n); if (t === 'script' || t === 'noscript' || n.hasAttribute('data-cm-secret') || (t === 'input' && String(n.getAttribute('type') || n.type || '').trim().toLowerCase() === 'password')) n.remove(); }}); if (cl.querySelectorAll) cl.querySelectorAll('script,noscript,[data-cm-secret],input[type=\"password\"]').forEach(n => n.remove()); return String(cl.innerText || cl.textContent || ''); }})()"
        );
        let val = self
            .lease
            .eval_json(&expr)
            .map_err(|e| self.map_op_err(e))?;
        Ok(truncate_utf8(
            &super::super::browser_html::sanitize_html(val.as_str().unwrap_or("")),
            cap,
        ))
    }

    fn attribute(&mut self, element: &str, name: &str) -> Result<Option<String>, ApiError> {
        self.ensure_live_session()?;
        self.ensure_same_origin_if_navigated()?;
        let dom_id = self.lookup_handle(element)?;
        let lower = name.trim().to_ascii_lowercase();
        if lower.is_empty() || lower.starts_with("on") || lower == "data-cm-secret" {
            return Err(api_err(
                ErrorCode::PermissionDenied,
                "attribute access denied",
            ));
        }
        let id_json = serde_json::to_string(&dom_id)
            .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
        let name_json = serde_json::to_string(&lower)
            .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
        let expr = format!(
            "/* __cm_attr */ (() => {{ const el = document.querySelector('[data-cm-handle=' + JSON.stringify({id_json}) + ']'); if (!el) return null; const tag = ((el.localName || el.tagName || '').split(':').pop() || '').toLowerCase(); const isSecret = el.hasAttribute('data-cm-secret') || (tag === 'input' && String(el.getAttribute('type') || el.type || '').trim().toLowerCase() === 'password'); if (isSecret && {name_json} === 'value') return ''; return el.getAttribute({name_json}); }})()"
        );
        let val = self
            .lease
            .eval_json(&expr)
            .map_err(|e| self.map_op_err(e))?;
        Ok(val.as_str().map(super::super::browser_html::sanitize_html))
    }

    fn click(&mut self, element: &str) -> Result<String, ApiError> {
        self.ensure_live_session()?;
        self.ensure_same_origin_if_navigated()?;
        let dom_id = self.lookup_handle(element)?;
        self.post_nav_or_click = true;
        self.lease.mark_post_nav_sent();
        let id_json = serde_json::to_string(&dom_id)
            .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
        let expr = format!(
            "/* __cm_click */ (() => {{ const el = document.querySelector('[data-cm-handle=' + JSON.stringify({id_json}) + ']'); if (!el) return false; el.click(); return true; }})()"
        );
        let ok = self
            .lease
            .eval_json(&expr)
            .map_err(|e| self.map_op_err(e))?;
        if ok.as_bool() == Some(false) {
            return Err(api_err(ErrorCode::NotFound, "element not found for click"));
        }
        self.has_navigated = true;
        self.element_handles.clear();
        self.verify_current_page_origin()?;
        let receipt = format!("bclick-{}", self.next_seq);
        self.next_seq += 1;
        Ok(receipt)
    }

    fn fill(&mut self, element: &str, value: BrowserFillValue) -> Result<(), ApiError> {
        let obs = self.ensure_live_session()?;
        self.ensure_same_origin_if_navigated()?;
        let dom_id = self.lookup_handle(element)?;
        let (secret_or_text, is_secret_slot) = match value {
            BrowserFillValue::Text(s) => (s, false),
            BrowserFillValue::CredentialSlot(slot) => {
                let stripped = super::super::ports::strip_site_prefix(&slot);
                if stripped.is_empty() {
                    return Err(api_err(
                        ErrorCode::InvalidArgument,
                        "credential slot must not be empty",
                    ));
                }
                if slot
                    .trim()
                    .get(..5)
                    .is_some_and(|p| p.eq_ignore_ascii_case("site:"))
                    && stripped != self.site_id.to_string()
                {
                    return Err(api_err(
                        ErrorCode::PermissionDenied,
                        "credential slot cannot reference another site",
                    ));
                }
                let secret = match stripped.to_ascii_lowercase().as_str() {
                    "site-cookie" => obs.site().cookie.clone(),
                    "site-api-key" => obs.site().api_key.clone(),
                    _ => obs
                        .site()
                        .api_key
                        .clone()
                        .or_else(|| obs.site().cookie.clone()),
                }
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| api_err(ErrorCode::NotFound, "site credential slot unavailable"))?;
                self.verify_current_page_origin()?;
                (secret, true)
            }
        };
        let id_json = serde_json::to_string(&dom_id)
            .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
        let val_json = serde_json::to_string(&secret_or_text)
            .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
        let expr = if is_secret_slot {
            format!(
                "/* __cm_fill */ (() => {{ const el = document.querySelector('[data-cm-handle=' + JSON.stringify({id_json}) + ']'); if (!el) return false; const tag = ((el.localName || el.tagName || '').split(':').pop() || '').toLowerCase(); if (tag !== 'input' && tag !== 'textarea') return 'bad-target'; el.setAttribute('data-cm-secret', '1'); if (el.hasAttribute('value')) el.setAttribute('value', ''); el.value = {val_json}; el.dispatchEvent(new Event('input', {{ bubbles: true }})); el.dispatchEvent(new Event('change', {{ bubbles: true }})); return true; }})()"
            )
        } else {
            format!(
                "/* __cm_fill */ (() => {{ const el = document.querySelector('[data-cm-handle=' + JSON.stringify({id_json}) + ']'); if (!el) return false; el.value = {val_json}; el.dispatchEvent(new Event('input', {{ bubbles: true }})); el.dispatchEvent(new Event('change', {{ bubbles: true }})); return true; }})()"
            )
        };
        let ok = self
            .lease
            .eval_json(&expr)
            .map_err(|e| self.map_op_err(e))?;
        if ok.as_str() == Some("bad-target") {
            return Err(api_err(
                ErrorCode::InvalidArgument,
                "credential slot can only fill input or textarea controls",
            ));
        }
        if ok.as_bool() == Some(false) {
            return Err(api_err(ErrorCode::NotFound, "element not found for fill"));
        }
        Ok(())
    }

    fn wait_for(
        &mut self,
        selector: Option<&str>,
        condition: &str,
        timeout_ms: u64,
    ) -> Result<String, ApiError> {
        let cap_ms = timeout_ms.clamp(1, 30_000);
        let wait_deadline = (Instant::now() + Duration::from_millis(cap_ms)).min(self.deadline);
        let sel_expr = match selector {
            Some(sel) => {
                if sel.trim().is_empty() || sel.len() > 512 || sel.chars().any(char::is_control) {
                    return Err(api_err(ErrorCode::InvalidArgument, "invalid wait selector"));
                }
                serde_json::to_string(sel)
                    .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?
            }
            None => "null".into(),
        };
        let cond_json = serde_json::to_string(condition)
            .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
        let expr = format!(
            "/* __cm_wait */ (() => {{ if ({sel_expr} !== null) {{ const el = document.querySelector({sel_expr}); if (!el) return false; if ({cond_json} === 'visible') return !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length); return true; }} return document.readyState === 'complete' || document.readyState === 'interactive'; }})()"
        );
        loop {
            self.ensure_live_session()?;
            self.ensure_same_origin_if_navigated()?;
            let ready = self
                .lease
                .eval_json(&expr)
                .map_err(|e| self.map_op_err(e))?;
            if ready.as_bool().unwrap_or(false) {
                let receipt = format!("bwait-{}", self.next_seq);
                self.next_seq += 1;
                return Ok(receipt);
            }
            if Instant::now() >= wait_deadline {
                return Err(if self.post_nav_or_click {
                    api_err(ErrorCode::UnknownOutcome, "browser wait_for timed out")
                } else {
                    api_err(ErrorCode::DeadlineExceeded, "browser wait_for timed out")
                });
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn read_response(&mut self, receipt: &str, max_bytes: u32) -> Result<HttpResponse, ApiError> {
        self.ensure_live_session()?;
        self.ensure_same_origin_if_navigated()?;
        let Some(resp) = self.nav_responses.get(receipt) else {
            return Err(api_err(ErrorCode::StaleHandle, "unknown browser receipt"));
        };
        let cap = (max_bytes as usize).clamp(1, MAX_BROWSER_BYTES);
        let (body, truncated) = match super::super::browser_html::decode_nav_body_for_sanitize(
            &resp.headers,
            &resp.body,
        ) {
            Some((decoded, explicit_xhtml)) => {
                let clean =
                    super::super::browser_html::sanitize_html_with_mode(&decoded, explicit_xhtml);
                let trunc = clean.len() > cap;
                (truncate_utf8(&clean, cap).into_bytes(), trunc)
            }
            None => match std::str::from_utf8(&resp.body) {
                Ok(text) => {
                    let trunc = text.len() > cap;
                    (truncate_utf8(text, cap).into_bytes(), trunc)
                }
                Err(_) => {
                    let trunc = resp.body.len() > cap;
                    (resp.body[..resp.body.len().min(cap)].to_vec(), trunc)
                }
            },
        };
        let headers = resp
            .headers
            .iter()
            .filter(|(k, _)| {
                let lower = k.trim().to_ascii_lowercase();
                !SENSITIVE_RESPONSE_HEADERS.contains(&lower.as_str())
            })
            .map(|(name, value)| Header {
                name: name.clone(),
                value: value.clone(),
            })
            .collect();
        Ok(HttpResponse {
            status: resp.status,
            headers,
            body,
            truncated,
            receipt: None,
        })
    }

    fn capture_session(&mut self) -> Result<String, ApiError> {
        if self.purpose != BrowserPurpose::Maintenance {
            return Err(api_err(
                ErrorCode::WrongContext,
                "capture_session is only allowed in maintenance purpose",
            ));
        }
        self.ensure_live_session()?;
        self.ensure_same_origin_if_navigated()?;
        let cookie_header = self
            .lease
            .cookie_header(&self.site_url)
            .map_err(|e| self.map_op_err(e))?;
        let token = self.ports.record_captured_cookies(CapturedCookieRecord {
            site_id: self.site_id,
            credential_epoch: self.credential_epoch.clone(),
            origin: self.site_origin.clone(),
            set_cookies: Vec::new(),
            merged_cookie: (!cookie_header.is_empty()).then_some(cookie_header),
        });
        Ok(token)
    }

    fn close(&mut self) -> Result<(), ApiError> {
        self.lease
            .close_verified()
            .map_err(|e| api_err(ErrorCode::Conflict, &e.message))
    }
}

fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}
