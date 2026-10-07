use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, client::IntoClientRequest};

use crate::IndexerError;

pub(super) const TIMEOUT: Duration = Duration::from_secs(5);

pub(super) fn failure(context: &str, error: impl std::fmt::Display) -> IndexerError {
    tracing::error!(context, error = %error, "CDP operation failed");
    IndexerError::Fetch(format!("CDP {context}: {error}"))
}

pub(super) fn remaining(deadline: Instant) -> Result<Duration, IndexerError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| failure("timeout", "operation deadline exceeded"))
}

pub(super) struct Transport {
    socket: WebSocket<TcpStream>,
    next_id: u64,
    failed: bool,
    pub(super) loaded_loaders: std::collections::HashSet<String>,
}

impl Transport {
    pub(super) fn connect(url: &str) -> Result<Self, IndexerError> {
        let deadline = Instant::now() + TIMEOUT;
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
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(|error| failure("handshake read timeout", error))?;
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(|error| failure("handshake write timeout", error))?;
        stream
            .set_nonblocking(true)
            .map_err(|error| failure("handshake mode", error))?;
        let mut handshake = tungstenite::client(request, stream);
        let socket = loop {
            match handshake {
                Ok((socket, _)) => break socket,
                Err(tungstenite::HandshakeError::Interrupted(mid)) => {
                    std::thread::sleep(remaining(deadline)?.min(Duration::from_millis(5)));
                    handshake = mid.handshake();
                }
                Err(error) => return Err(failure("WebSocket handshake", error)),
            }
        };
        remaining(deadline)?;
        Ok(Self {
            socket,
            next_id: 0,
            failed: false,
            loaded_loaders: std::collections::HashSet::new(),
        })
    }

    pub(super) fn command(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, IndexerError> {
        if self.failed {
            return Err(failure(method, "transport is no longer usable"));
        }
        let result = self.exchange(method, params, deadline);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn exchange(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, IndexerError> {
        self.next_id += 1;
        let id = self.next_id;
        self.set_deadline(deadline)?;
        let request = json!({"id":id, "method":method, "params":params});
        self.send(request, method, deadline)?;
        loop {
            self.set_deadline(deadline)?;
            let message = match self.socket.read() {
                Ok(message) => message,
                Err(tungstenite::Error::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    std::thread::sleep(remaining(deadline)?.min(Duration::from_millis(5)));
                    continue;
                }
                Err(error) => return Err(failure(method, error)),
            };
            match message {
                Message::Text(text) => {
                    let response: Value = serde_json::from_str(&text)
                        .map_err(|error| failure("invalid protocol JSON", error))?;
                    if response["method"] == "Page.lifecycleEvent"
                        && response["params"]["name"] == "load"
                    {
                        if let Some(loader) = response["params"]["loaderId"].as_str() {
                            self.loaded_loaders.insert(loader.to_owned());
                        }
                    }
                    if response["id"].as_u64() != Some(id) {
                        continue;
                    }
                    if let Some(error) = response.get("error") {
                        return Err(failure(method, error));
                    }
                    return response
                        .get("result")
                        .cloned()
                        .ok_or_else(|| failure(method, "missing command result"));
                }
                Message::Close(_) => return Err(failure(method, "WebSocket closed")),
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
            Ok(()) => {}
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock =>
            {
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

    fn set_deadline(&self, deadline: Instant) -> Result<(), IndexerError> {
        let timeout = Some(remaining(deadline)?);
        self.socket
            .get_ref()
            .set_read_timeout(timeout)
            .map_err(|error| failure("read timeout", error))?;
        self.socket
            .get_ref()
            .set_write_timeout(timeout)
            .map_err(|error| failure("write timeout", error))
    }

    pub(super) fn close(&mut self) {
        // Target cleanup must still run if the peer cannot complete a close handshake.
        if let Err(error) = self.socket.get_ref().shutdown(std::net::Shutdown::Both) {
            tracing::warn!(%error, "CDP socket cleanup failed");
        }
    }
}

fn connect_tcp(host: String, port: u16, deadline: Instant) -> Result<TcpStream, IndexerError> {
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
