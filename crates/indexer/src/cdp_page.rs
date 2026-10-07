use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::IndexerError;
use crate::browser::PageSession;

#[path = "cdp_page_transport.rs"]
mod transport;
use transport::{TIMEOUT, Transport, failure, remaining};

pub struct CdpPageSession {
    base_url: String,
    target_id: String,
    transport: Mutex<Option<Transport>>,
}

impl CdpPageSession {
    fn with_transport<T>(
        &self,
        operation: impl FnOnce(&mut Transport, Instant) -> Result<T, IndexerError>,
    ) -> Result<T, IndexerError> {
        let deadline = Instant::now() + TIMEOUT;
        // A stalled caller cannot make subsequent callers wait indefinitely for the mutex.
        loop {
            match self.transport.try_lock() {
                Ok(mut guard) => {
                    let transport = guard
                        .as_mut()
                        .ok_or_else(|| failure("transport", "connection not initialized"))?;
                    return operation(transport, deadline);
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(failure("transport", "mutex poisoned"));
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(remaining(deadline)?.min(Duration::from_millis(10)));
                }
            }
        }
    }
}

impl PageSession for CdpPageSession {
    fn goto(&self, url: &str) -> Result<(), IndexerError> {
        self.with_transport(|transport, deadline| {
            transport.loaded_loaders.clear();
            let result = transport.command("Page.navigate", json!({"url":url}), deadline)?;
            if let Some(error) = result.get("errorText") {
                return Err(failure("navigation", error));
            }
            let frame_id = result.get("frameId").and_then(Value::as_str);
            if frame_id.is_none() || result["isDownload"].as_bool() == Some(true) {
                return Err(failure("navigation", "no document navigation"));
            }
            loop {
                let state = evaluate(transport, "document.readyState", deadline)?;
                let loader = result["loaderId"].as_str();
                let loaded = loader.is_none_or(|id| transport.loaded_loaders.contains(id));
                if state == "complete" && loaded {
                    return Ok(());
                }
                if state != "loading" && state != "interactive" && state != "complete" {
                    return Err(failure("document readiness", state));
                }
                std::thread::sleep(remaining(deadline)?.min(Duration::from_millis(50)));
            }
        })
    }

    fn set_cookie_header(&self, header: &str) -> Result<(), IndexerError> {
        self.with_transport(|transport, deadline| {
            transport.command(
                "Network.setExtraHTTPHeaders",
                json!({"headers":{"Cookie":header}}),
                deadline,
            )?;
            Ok(())
        })
    }

    fn content(&self) -> Result<String, IndexerError> {
        self.with_transport(|transport, deadline| {
            evaluate(transport, "document.documentElement.outerHTML", deadline)
        })
    }
}

fn evaluate(
    transport: &mut Transport,
    expression: &str,
    deadline: Instant,
) -> Result<String, IndexerError> {
    let result = transport.command(
        "Runtime.evaluate",
        json!({"expression":expression,"returnByValue":true}),
        deadline,
    )?;
    if let Some(exception) = result.get("exceptionDetails") {
        return Err(failure("evaluation exception", exception));
    }
    result["result"]["value"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| failure("evaluation", "expected string result"))
}

impl Drop for CdpPageSession {
    fn drop(&mut self) {
        if let Ok(Some(transport)) = self.transport.get_mut().map(Option::as_mut) {
            transport.close();
        }
        let close_url = format!("{}/json/close/{}", self.base_url, self.target_id);
        if let Err(error) = http_agent(Duration::from_secs(3))
            .put(&close_url)
            .send_empty()
        {
            tracing::warn!(target_id = %self.target_id, %error, "CDP target cleanup failed");
        }
    }
}

fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .new_agent()
}

pub fn open_cdp_session(cdp_url: &str) -> Result<CdpPageSession, IndexerError> {
    let base = cdp_url.trim_end_matches('/');
    let new_url = format!("{base}/json/new");
    let response: Value = http_agent(TIMEOUT)
        .put(&new_url)
        .send_empty()
        .map_err(|error| failure("无法连接外部 CDP 端点创建 target", error))?
        .into_body()
        .read_json()
        .map_err(|error| failure("target JSON", error))?;
    let target_id = response["id"]
        .as_str()
        .ok_or_else(|| failure("create target", "missing target id"))?
        .to_owned();
    // Install cleanup before any operation that can fail after the target is created.
    let mut session = CdpPageSession {
        base_url: base.to_owned(),
        target_id,
        transport: Mutex::new(None),
    };
    let ws_url = response["webSocketDebuggerUrl"]
        .as_str()
        .ok_or_else(|| failure("create target", "missing WebSocket URL"))?;
    let mut transport = Transport::connect(ws_url)?;
    let deadline = Instant::now() + TIMEOUT;
    transport.command("Network.enable", json!({}), deadline)?;
    transport.command("Page.enable", json!({}), deadline)?;
    transport.command(
        "Page.setLifecycleEventsEnabled",
        json!({"enabled":true}),
        deadline,
    )?;
    *session
        .transport
        .get_mut()
        .map_err(|_| failure("transport", "mutex poisoned"))? = Some(transport);
    tracing::debug!(target_id = %session.target_id, "CDP page session opened");
    Ok(session)
}
