use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, client::IntoClientRequest};

use crate::IndexerError;

pub(crate) const TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn failure(context: &str, error: impl std::fmt::Display) -> IndexerError {
    tracing::error!(context, error = %error, "CDP operation failed");
    IndexerError::Fetch(format!("CDP {context}: {error}"))
}

pub(crate) fn remaining(deadline: Instant) -> Result<Duration, IndexerError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| failure("timeout", "operation deadline exceeded"))
}

pub(crate) struct CapturedDocResponse {
    pub request_id: String,
    pub frame_id: Option<String>,
    pub loader_id: Option<String>,
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

pub(crate) struct Transport {
    socket: WebSocket<TcpStream>,
    next_id: u64,
    sent_count: u64,
    failed: bool,
    io_broken: bool,
    pub(crate) loaded_loaders: std::collections::HashSet<String>,
    pub(crate) doc_responses: Vec<CapturedDocResponse>,
    /// Flat-mode target session (`Target.attachToTarget{flatten:true}`); page
    /// commands carry it and only its events are tracked.
    pub(crate) session_id: Option<String>,
    protocol_error: bool,
}

/// CDP "method not found / no such page" (`-32601`).
pub(crate) fn is_method_not_found(error: &IndexerError) -> bool {
    error.to_string().contains("-32601")
}

impl Transport {
    pub(super) fn connect(url: &str, caller_deadline: Instant) -> Result<Self, IndexerError> {
        let deadline = caller_deadline.min(Instant::now() + TIMEOUT);
        let request = url
            .into_client_request()
            .map_err(|error| failure("WebSocket URL", error))?;
        if request.uri().scheme_str() != Some("ws") {
            return Err(failure(
                "WebSocket URL",
                "only ws is supported by this build",
            ));
        }
        let host = request
            .uri()
            .host()
            .ok_or_else(|| failure("WebSocket URL", "missing host"))?;
        let host = host.trim_matches(['[', ']']).to_string();
        let port = request.uri().port_u16().unwrap_or(80);
        let stream = connect_tcp(host, port, deadline)?;
        let initial_timeout = remaining(deadline)?.max(Duration::from_millis(1));
        stream
            .set_read_timeout(Some(initial_timeout))
            .map_err(|error| failure("handshake read timeout", error))?;
        stream
            .set_write_timeout(Some(initial_timeout))
            .map_err(|error| failure("handshake write timeout", error))?;
        let mut handshake = tungstenite::client(request, stream);
        let socket = loop {
            match handshake {
                Ok((socket, _)) => break socket,
                Err(tungstenite::HandshakeError::Interrupted(mid)) => {
                    let t = remaining(deadline)?.max(Duration::from_millis(1));
                    let _ = mid.get_ref().get_ref().set_read_timeout(Some(t));
                    let _ = mid.get_ref().get_ref().set_write_timeout(Some(t));
                    handshake = mid.handshake();
                }
                Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Io(err)))
                    if err.kind() == std::io::ErrorKind::TimedOut =>
                {
                    remaining(deadline)?;
                    return Err(failure("WebSocket handshake", err));
                }
                Err(error) => return Err(failure("WebSocket handshake", error)),
            }
        };
        remaining(deadline)?;
        Ok(Self {
            socket,
            next_id: 0,
            sent_count: 0,
            failed: false,
            io_broken: false,
            loaded_loaders: std::collections::HashSet::new(),
            doc_responses: Vec::new(),
            session_id: None,
            protocol_error: false,
        })
    }

    pub(crate) fn sent_count(&self) -> u64 {
        self.sent_count
    }

    /// Page-level command (sent on the attached session when there is one).
    pub(crate) fn command(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, IndexerError> {
        let session = self.session_id.clone();
        self.command_on(session.as_deref(), method, params, deadline)
    }

    /// Browser-level command (never carries a sessionId).
    pub(crate) fn command_root(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, IndexerError> {
        self.command_on(None, method, params, deadline)
    }

    /// Browser-level cleanup command (`Target.closeTarget`,
    /// `Target.disposeBrowserContext`). Runs even after a command deadline
    /// timeout (`self.failed`) as long as WebSocket I/O is intact (`!self.io_broken`).
    pub(crate) fn command_cleanup_root(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, IndexerError> {
        if self.io_broken {
            return Err(failure(method, "transport is no longer usable"));
        }
        remaining(deadline)?;
        self.protocol_error = false;
        let result = self.exchange(None, method, params, deadline);
        if result.is_err() && !self.protocol_error {
            self.failed = true;
        }
        result
    }

    fn command_on(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, IndexerError> {
        if self.failed || self.io_broken {
            return Err(failure(method, "transport is no longer usable"));
        }
        remaining(deadline)?;
        self.protocol_error = false;
        let result = self.exchange(session, method, params, deadline);
        // A CDP error *response* leaves the connection usable (cleanup can
        // still run); I/O errors and timeouts poison normal commands.
        if result.is_err() && !self.protocol_error {
            self.failed = true;
        }
        result
    }

    fn track_event(&mut self, response: &Value) {
        let event_session = response["sessionId"].as_str();
        let ours = match (&self.session_id, event_session) {
            (Some(mine), Some(theirs)) => mine == theirs,
            _ => true,
        };
        if !ours {
            return;
        }
        if response["method"] == "Page.lifecycleEvent"
            && response["params"]["name"] == "load"
            && let Some(loader) = response["params"]["loaderId"].as_str()
        {
            self.loaded_loaders.insert(loader.to_owned());
        }
        if response["method"] == "Network.responseReceived"
            && response["params"]["type"].as_str() == Some("Document")
            && let Some(req_id) = response["params"]["requestId"].as_str()
        {
            let status = response["params"]["response"]["status"]
                .as_u64()
                .and_then(|s| u16::try_from(s).ok())
                .unwrap_or(200);
            let headers = response["params"]["response"]["headers"]
                .as_object()
                .map(|map| {
                    map.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            self.doc_responses.push(CapturedDocResponse {
                request_id: req_id.to_string(),
                frame_id: response["params"]["frameId"].as_str().map(str::to_string),
                loader_id: response["params"]["loaderId"].as_str().map(str::to_string),
                status,
                headers,
            });
        }
    }

    fn exchange(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, IndexerError> {
        self.next_id += 1;
        let id = self.next_id;
        let timeout = remaining(deadline)?;
        if let Err(err) = self.set_socket_timeout(timeout) {
            self.io_broken = true;
            return Err(err);
        }
        let mut request = json!({"id":id, "method":method, "params":params});
        if let Some(session) = session {
            request["sessionId"] = json!(session);
        }
        if let Err(err) = self.send(request, method, deadline) {
            self.io_broken = true;
            return Err(err);
        }
        loop {
            let timeout = remaining(deadline)?;
            if let Err(err) = self.set_socket_timeout(timeout) {
                self.io_broken = true;
                return Err(err);
            }
            let message = match self.socket.read() {
                Ok(message) => message,
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    remaining(deadline)?;
                    continue;
                }
                Err(error) => {
                    self.io_broken = true;
                    return Err(failure(method, error));
                }
            };
            match message {
                Message::Text(text) => {
                    let response: Value = serde_json::from_str(&text).map_err(|error| {
                        self.io_broken = true;
                        failure("invalid protocol JSON", error)
                    })?;
                    self.track_event(&response);
                    if response["id"].as_u64() != Some(id) {
                        continue;
                    }
                    if let Some(error) = response.get("error") {
                        self.protocol_error = true;
                        return Err(failure(method, error));
                    }
                    return response.get("result").cloned().ok_or_else(|| {
                        self.io_broken = true;
                        failure(method, "missing command result")
                    });
                }
                Message::Close(_) => {
                    self.io_broken = true;
                    return Err(failure(method, "WebSocket closed"));
                }
                _ => continue,
            }
        }
    }

    fn send(
        &mut self,
        request: Value,
        method: &str,
        deadline: Instant,
    ) -> Result<(), IndexerError> {
        match self.socket.send(Message::Text(request.to_string().into())) {
            Ok(()) => {
                self.sent_count = self.sent_count.saturating_add(1);
            }
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                self.sent_count = self.sent_count.saturating_add(1);
                loop {
                    std::thread::sleep(remaining(deadline)?.min(Duration::from_millis(5)));
                    match self.socket.flush() {
                        Ok(()) => break,
                        Err(tungstenite::Error::Io(error))
                            if error.kind() == std::io::ErrorKind::WouldBlock =>
                        {
                            continue;
                        }
                        Err(error) => return Err(failure(method, error)),
                    }
                }
            }
            Err(error) => return Err(failure(method, error)),
        }
        Ok(())
    }

    fn set_socket_timeout(&self, timeout: Duration) -> Result<(), IndexerError> {
        let clamped = timeout.max(Duration::from_millis(1));
        self.socket
            .get_ref()
            .set_read_timeout(Some(clamped))
            .map_err(|error| failure("read timeout", error))?;
        self.socket
            .get_ref()
            .set_write_timeout(Some(clamped))
            .map_err(|error| failure("write timeout", error))
    }

    pub(crate) fn close(&mut self) {
        // Target cleanup must still run if the peer cannot complete a close handshake.
        if let Err(error) = self.socket.get_ref().shutdown(std::net::Shutdown::Both) {
            tracing::warn!(%error, "CDP socket cleanup failed");
        }
    }
}

fn connect_tcp(host: String, port: u16, deadline: Instant) -> Result<TcpStream, IndexerError> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let addr = std::net::SocketAddr::new(ip, port);
        return TcpStream::connect_timeout(&addr, remaining(deadline)?)
            .map_err(|error| failure("TCP connect", error));
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let addresses = (host.as_str(), port)
            .to_socket_addrs()
            .map(|addresses| addresses.collect::<Vec<_>>());
        let _ = sender.send(addresses);
    });
    let addresses = receiver
        .recv_timeout(remaining(deadline)?)
        .map_err(|error| failure("DNS timeout", error))?
        .map_err(|error| failure("DNS resolution", error))?;
    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, remaining(deadline)?) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(failure(
        "TCP connect",
        last_error.map_or_else(|| "no resolved addresses".into(), |error| error.to_string()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[test]
    fn exchange_pre_send_deadline_expiry_preserves_cleanup_transport() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_srv = Arc::clone(&seen);
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            while let Ok(msg) = ws.read() {
                if let Message::Text(text) = msg {
                    let cmd: Value = serde_json::from_str(&text).unwrap();
                    seen_srv
                        .lock()
                        .unwrap()
                        .push(cmd["method"].as_str().unwrap().to_string());
                    let reply = json!({ "id": cmd["id"], "result": { "success": true } });
                    let _ = ws.send(Message::Text(reply.to_string().into()));
                }
            }
        });

        let mut transport = Transport::connect(
            &format!("ws://{addr}/devtools/browser"),
            Instant::now() + TIMEOUT,
        )
        .unwrap();
        let expired = Instant::now() - Duration::from_millis(1);
        let err = transport
            .exchange(
                None,
                "Page.navigate",
                json!({ "url": "https://site.invalid" }),
                expired,
            )
            .unwrap_err();
        assert!(err.to_string().contains("operation deadline exceeded"));
        assert!(seen.lock().unwrap().is_empty());

        transport
            .command_cleanup_root(
                "Target.disposeBrowserContext",
                json!({ "browserContextId": "ctx" }),
                Instant::now() + Duration::from_secs(2),
            )
            .expect("pre-send deadline expiry in exchange must leave transport usable for cleanup");
        transport.close();
        server.join().unwrap();
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["Target.disposeBrowserContext"]
        );
    }
}
