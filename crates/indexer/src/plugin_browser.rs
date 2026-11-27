use std::time::Instant;

use serde_json::{Value, json};

use crate::IndexerError;
use crate::browser::browser_slots::SlotGuard;
use crate::browser::{Browser, PageSession};
use crate::cdp_page::{
    CdpPageSession, INCOMPATIBLE_PREFIX, evaluate, evaluate_json, open_cdp_session_inner,
    transport::{failure, is_method_not_found, remaining},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginNavResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginBrowserErrorKind {
    Setup,
    Incompatible,
    Busy,
    InvalidArgument,
    NotFound,
    PostNavUnknown,
    Transport,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct PluginBrowserError {
    pub kind: PluginBrowserErrorKind,
    pub message: String,
}

impl PluginBrowserError {
    pub fn new(kind: PluginBrowserErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

pub trait PluginBrowserPage: Send + Sync {
    fn set_cookies(
        &mut self,
        page_url: &str,
        cookie_header: &str,
    ) -> Result<(), PluginBrowserError>;
    fn navigate(
        &mut self,
        url: &str,
        wait_until: &str,
    ) -> Result<PluginNavResponse, PluginBrowserError>;
    fn eval_json(&mut self, expression: &str) -> Result<Value, PluginBrowserError>;
    fn cookie_header(&mut self, page_url: &str) -> Result<String, PluginBrowserError>;
    fn close_verified(&mut self) -> Result<(), PluginBrowserError>;
    fn mark_post_nav_sent(&mut self) {}
}

pub struct PluginBrowserLease {
    page: Box<dyn PluginBrowserPage>,
    _slot: Option<SlotGuard>,
    closed: bool,
}

impl PluginBrowserLease {
    pub fn set_cookies(
        &mut self,
        page_url: &str,
        cookie_header: &str,
    ) -> Result<(), PluginBrowserError> {
        self.page.set_cookies(page_url, cookie_header)
    }

    pub fn navigate(
        &mut self,
        url: &str,
        wait_until: &str,
    ) -> Result<PluginNavResponse, PluginBrowserError> {
        self.page.navigate(url, wait_until)
    }

    pub fn eval_json(&mut self, expression: &str) -> Result<Value, PluginBrowserError> {
        self.page.eval_json(expression)
    }

    pub fn cookie_header(&mut self, page_url: &str) -> Result<String, PluginBrowserError> {
        self.page.cookie_header(page_url)
    }

    pub fn mark_post_nav_sent(&mut self) {
        self.page.mark_post_nav_sent();
    }

    pub fn close_verified(&mut self) -> Result<(), PluginBrowserError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let res = self.page.close_verified();
        drop(self._slot.take());
        res
    }
}

impl Drop for PluginBrowserLease {
    fn drop(&mut self) {
        if !self.closed {
            self.closed = true;
            let _ = self.page.close_verified();
        }
        drop(self._slot.take());
    }
}

pub struct CdpPluginPage {
    session: CdpPageSession,
    post_nav_sent: bool,
    closed: bool,
}

impl CdpPluginPage {
    pub fn open(
        cdp_url: &str,
        deadline: Instant,
        target_close_cap: std::time::Duration,
    ) -> Result<Self, PluginBrowserError> {
        let session =
            open_cdp_session_inner(cdp_url, deadline, target_close_cap).map_err(map_open_error)?;
        Ok(Self {
            session,
            post_nav_sent: false,
            closed: false,
        })
    }

    fn map_op_err(&self, err: IndexerError) -> PluginBrowserError {
        if self.post_nav_sent {
            PluginBrowserError::new(PluginBrowserErrorKind::PostNavUnknown, err.to_string())
        } else {
            map_open_error(err)
        }
    }
}

fn select_main_doc_response(
    responses: &mut Vec<crate::cdp_page::transport::CapturedDocResponse>,
    frame_id: Option<&str>,
    loader_id: Option<&str>,
) -> Option<crate::cdp_page::transport::CapturedDocResponse> {
    let drained: Vec<_> = std::mem::take(responses);
    if let Some(fid) = frame_id
        && let Some(pos) = drained
            .iter()
            .rposition(|r| r.frame_id.as_deref() == Some(fid))
    {
        return drained.into_iter().nth(pos);
    }
    if let Some(lid) = loader_id
        && let Some(pos) = drained
            .iter()
            .rposition(|r| r.loader_id.as_deref() == Some(lid))
    {
        return drained.into_iter().nth(pos);
    }
    drained
        .into_iter()
        .rfind(|r| r.frame_id.is_none() && r.loader_id.is_none())
}

fn normalize_outerhtml_headers(headers: &mut Vec<(String, String)>) {
    headers.retain(|(name, _)| {
        !name.eq_ignore_ascii_case("content-encoding")
            && !name.eq_ignore_ascii_case("content-length")
    });
    let mut found_ct = false;
    for (name, value) in headers.iter_mut() {
        if !name.eq_ignore_ascii_case("content-type") {
            continue;
        }
        found_ct = true;
        let media_type = value
            .split(';')
            .next()
            .map(str::trim)
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            media_type.as_str(),
            "application/xhtml+xml" | "application/xml" | "text/xml"
        ) {
            *value = format!("{media_type}; charset=utf-8");
        } else {
            *value = "text/html; charset=utf-8".to_string();
        }
    }
    if !found_ct {
        headers.push((
            "Content-Type".to_string(),
            "text/html; charset=utf-8".to_string(),
        ));
    }
}

fn decode_cdp_response_body(v: &Value) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let raw = v["body"].as_str()?;
    if v["base64Encoded"].as_bool() == Some(true) {
        base64::engine::general_purpose::STANDARD.decode(raw).ok()
    } else {
        Some(raw.as_bytes().to_vec())
    }
}

impl PluginBrowserPage for CdpPluginPage {
    fn set_cookies(
        &mut self,
        page_url: &str,
        cookie_header: &str,
    ) -> Result<(), PluginBrowserError> {
        self.session
            .set_site_cookies(page_url, cookie_header)
            .map_err(|e| self.map_op_err(e))
    }

    fn navigate(
        &mut self,
        url: &str,
        wait_until: &str,
    ) -> Result<PluginNavResponse, PluginBrowserError> {
        let mut nav_transmitted = false;
        let result = self.session.with_transport(|transport, deadline| {
            transport.loaded_loaders.clear();
            transport.doc_responses.clear();
            let before = transport.sent_count();
            let nav_res = transport.command("Page.navigate", json!({"url": url}), deadline);
            let transmitted = transport.sent_count() > before;
            let nav_res = nav_res.map_err(|error| {
                if is_method_not_found(&error) {
                    IndexerError::BrowserSetup(format!("Page.navigate unsupported: {error}"))
                } else {
                    nav_transmitted = transmitted;
                    error
                }
            })?;
            nav_transmitted = transmitted;
            if let Some(error) = nav_res.get("errorText") {
                return Err(failure("navigation", error));
            }
            let frame_id = nav_res.get("frameId").and_then(Value::as_str);
            if frame_id.is_none() || nav_res["isDownload"].as_bool() == Some(true) {
                return Err(failure("navigation", "no document navigation"));
            }
            let initial_loader = nav_res["loaderId"].as_str();
            loop {
                remaining(deadline)?;
                let state = evaluate(transport, "document.readyState", deadline)?;
                let active_loader = frame_id
                    .and_then(|fid| {
                        transport
                            .doc_responses
                            .iter()
                            .rev()
                            .find(|r| r.frame_id.as_deref() == Some(fid))
                            .and_then(|r| r.loader_id.as_deref())
                    })
                    .or(initial_loader);
                let loaded = active_loader.is_none_or(|id| transport.loaded_loaders.contains(id));
                let ready = match wait_until {
                    "domcontentloaded" => state == "interactive" || state == "complete",
                    _ => state == "complete" && loaded,
                };
                if ready {
                    break;
                }
                if state != "loading" && state != "interactive" && state != "complete" {
                    return Err(failure("document readiness", state));
                }
                std::thread::sleep(remaining(deadline)?.min(std::time::Duration::from_millis(50)));
            }

            let doc_meta =
                select_main_doc_response(&mut transport.doc_responses, frame_id, initial_loader);
            let (status, doc_headers, body_from_net) = if let Some(meta) = doc_meta {
                let net_body = transport
                    .command(
                        "Network.getResponseBody",
                        json!({"requestId": meta.request_id}),
                        deadline,
                    )
                    .ok()
                    .and_then(|v| decode_cdp_response_body(&v));
                (meta.status, meta.headers, net_body)
            } else {
                (200, Vec::new(), None)
            };

            let mut headers = doc_headers;
            let body = match body_from_net {
                Some(b) => b,
                None => {
                    normalize_outerhtml_headers(&mut headers);
                    evaluate(transport, "document.documentElement.outerHTML", deadline)?
                        .into_bytes()
                }
            };
            Ok(PluginNavResponse {
                status,
                headers,
                body,
            })
        });

        if nav_transmitted {
            self.post_nav_sent = true;
        }
        result.map_err(|err| {
            if is_method_not_found(&err) && err.to_string().contains("Page.navigate unsupported") {
                PluginBrowserError::new(PluginBrowserErrorKind::Setup, err.to_string())
            } else {
                self.map_op_err(err)
            }
        })
    }

    fn eval_json(&mut self, expression: &str) -> Result<Value, PluginBrowserError> {
        self.session
            .with_transport(|transport, deadline| evaluate_json(transport, expression, deadline))
            .map_err(|e| self.map_op_err(e))
    }

    fn cookie_header(&mut self, page_url: &str) -> Result<String, PluginBrowserError> {
        self.session
            .cookie_header(page_url)
            .map_err(|e| self.map_op_err(e))
    }

    fn close_verified(&mut self) -> Result<(), PluginBrowserError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.session
            .close_verified_inner()
            .map_err(|e| PluginBrowserError::new(PluginBrowserErrorKind::Transport, e.to_string()))
    }

    fn mark_post_nav_sent(&mut self) {
        self.post_nav_sent = true;
    }
}

impl Drop for CdpPluginPage {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.close_verified();
        }
    }
}

fn map_open_error(err: IndexerError) -> PluginBrowserError {
    match err {
        IndexerError::BrowserBusy => {
            PluginBrowserError::new(PluginBrowserErrorKind::Busy, err.to_string())
        }
        IndexerError::BrowserSetup(msg) if msg.starts_with(INCOMPATIBLE_PREFIX) => {
            PluginBrowserError::new(PluginBrowserErrorKind::Incompatible, msg)
        }
        IndexerError::BrowserSetup(msg) => {
            PluginBrowserError::new(PluginBrowserErrorKind::Setup, msg)
        }
        other => PluginBrowserError::new(PluginBrowserErrorKind::Transport, other.to_string()),
    }
}

pub(crate) fn open_plugin_session_inner(
    browser: &Browser,
    site_url: &str,
    cdp_url: Option<&str>,
    deadline: Instant,
) -> Result<PluginBrowserLease, PluginBrowserError> {
    browser.validate_timing().map_err(map_open_error)?;
    let uri: tungstenite::http::Uri = site_url.parse().map_err(|e| {
        PluginBrowserError::new(
            PluginBrowserErrorKind::InvalidArgument,
            format!("invalid site URL: {e}"),
        )
    })?;
    let host = uri.host().ok_or_else(|| {
        PluginBrowserError::new(PluginBrowserErrorKind::InvalidArgument, "missing site host")
    })?;
    let group = crate::browser_host::try_group_key(host).map_err(map_open_error)?;

    let config = browser.effective_config().map_err(map_open_error)?;
    let (endpoint, _) = config
        .resolve(cdp_url)
        .map_err(map_open_error)?
        .ok_or_else(|| PluginBrowserError::new(PluginBrowserErrorKind::Setup, "CDP disabled"))?;
    if let Some(reason) = browser.incompatible_endpoints().get(&endpoint) {
        return Err(PluginBrowserError::new(
            PluginBrowserErrorKind::Incompatible,
            reason.clone(),
        ));
    }

    let effective_deadline = deadline.min(Instant::now() + browser.render_deadline());
    let slot = browser
        .slots()
        .acquire(
            &group,
            effective_deadline,
            browser.queue_cap(),
            browser.min_work(),
        )
        .map_err(map_open_error)?;

    let page_res = match browser.plugin_opener() {
        Some(opener) => opener(Some(&endpoint)),
        None => CdpPluginPage::open(&endpoint, effective_deadline, browser.target_close_cap())
            .map(|p| Box::new(p) as Box<dyn PluginBrowserPage>),
    };

    let page = match page_res {
        Ok(page) => page,
        Err(err) => {
            if err.kind == PluginBrowserErrorKind::Incompatible {
                browser.record_incompatible(endpoint, err.message.clone());
            }
            drop(slot);
            return Err(err);
        }
    };

    Ok(PluginBrowserLease {
        page,
        _slot: Some(slot),
        closed: false,
    })
}

pub(crate) fn check_plugin_cdp_context_inner(
    browser: &Browser,
    cdp_url: Option<&str>,
    deadline: Instant,
) -> Result<(), PluginBrowserError> {
    static NEXT_PROBE_GROUP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

    browser.validate_timing().map_err(map_open_error)?;
    let config = browser.effective_config().map_err(map_open_error)?;
    let (endpoint, _) = config
        .resolve(cdp_url)
        .map_err(map_open_error)?
        .ok_or_else(|| PluginBrowserError::new(PluginBrowserErrorKind::Setup, "CDP disabled"))?;
    if let Some(reason) = browser.incompatible_endpoints().get(&endpoint) {
        return Err(PluginBrowserError::new(
            PluginBrowserErrorKind::Incompatible,
            reason.clone(),
        ));
    }

    let effective_deadline = deadline.min(Instant::now() + browser.render_deadline());
    let probe_group = format!(
        "cdp-check:{}",
        NEXT_PROBE_GROUP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let probe_min_work = browser
        .min_work()
        .min(std::time::Duration::from_millis(200));
    let _slot = browser
        .slots()
        .acquire(
            &probe_group,
            effective_deadline,
            browser.queue_cap(),
            probe_min_work,
        )
        .map_err(map_open_error)?;
    let page_res = match browser.plugin_opener() {
        Some(opener) => opener(Some(&endpoint)),
        None => CdpPluginPage::open(&endpoint, effective_deadline, browser.target_close_cap())
            .map(|p| Box::new(p) as Box<dyn PluginBrowserPage>),
    };
    let mut page = match page_res {
        Ok(page) => page,
        Err(err) => {
            if err.kind == PluginBrowserErrorKind::Incompatible {
                browser.record_incompatible(endpoint, err.message.clone());
            }
            return Err(err);
        }
    };
    page.close_verified()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::browser::BrowserConfig;

    struct NoopPluginPage;

    impl PluginBrowserPage for NoopPluginPage {
        fn set_cookies(&mut self, _: &str, _: &str) -> Result<(), PluginBrowserError> {
            Ok(())
        }
        fn navigate(&mut self, _: &str, _: &str) -> Result<PluginNavResponse, PluginBrowserError> {
            Ok(PluginNavResponse {
                status: 200,
                headers: Vec::new(),
                body: Vec::new(),
            })
        }
        fn eval_json(&mut self, _: &str) -> Result<Value, PluginBrowserError> {
            Ok(Value::Null)
        }
        fn cookie_header(&mut self, _: &str) -> Result<String, PluginBrowserError> {
            Ok(String::new())
        }
        fn close_verified(&mut self) -> Result<(), PluginBrowserError> {
            Ok(())
        }
    }

    #[test]
    fn check_plugin_cdp_context_validates_timing_and_respects_slot_capacity() {
        let invalid = Browser::new(BrowserConfig {
            enabled: true,
            endpoint: Some("http://127.0.0.1:9222".into()),
            managed_endpoint: None,
        })
        .with_render_deadline(Duration::from_secs(5))
        .with_min_work(Duration::from_secs(10));
        let timing_err = invalid
            .check_plugin_cdp_context(None, Instant::now() + Duration::from_secs(5))
            .unwrap_err();
        assert_eq!(timing_err.kind, PluginBrowserErrorKind::Setup);

        let capped = Browser::new(BrowserConfig {
            enabled: true,
            endpoint: Some("http://127.0.0.1:9222".into()),
            managed_endpoint: None,
        })
        .with_max_sessions(1)
        .with_queue_cap(Duration::from_millis(60))
        .with_min_work(Duration::from_millis(100))
        .with_plugin_opener(|_| Ok(Box::new(NoopPluginPage)));

        let lease = capped
            .open_plugin_session(
                "https://pt.example/",
                None,
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
        let busy_err = capped
            .check_plugin_cdp_context(None, Instant::now() + Duration::from_secs(5))
            .unwrap_err();
        assert_eq!(busy_err.kind, PluginBrowserErrorKind::Busy);

        drop(lease);
        capped
            .check_plugin_cdp_context(None, Instant::now() + Duration::from_secs(5))
            .unwrap();
    }
}
