//! Test-only lifecycle helpers shared by api and downloader tests (`#[path]`
//! included): a gate released on every exit path, and a fake HTTP server that
//! serves connections concurrently and stops and joins on drop.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// A one-shot gate a fixture blocks on until the test releases it.
#[derive(Default)]
pub struct Gate {
    released: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    /// Waits until released or `limit` passes; returns whether it was released.
    pub fn wait(&self, limit: Duration) -> bool {
        let guard = self.released.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, limit, |released| !*released)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }

    pub fn release(&self) {
        *self.released.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.changed.notify_all();
    }

    pub fn is_released(&self) -> bool {
        *self.released.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Owns the gate and releases it when dropped: success, `?`, panic or cancel.
pub struct ReleaseOnDrop(pub Arc<Gate>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Raw request bytes and the parsed head, handed to a [`FakeServer`] handler.
pub struct Request {
    pub raw: String,
    pub body: Vec<u8>,
}

/// Reads one HTTP request (head + Content-Length body); `None` on timeout/EOF.
pub fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut bytes = Vec::new();
    let mut expected = None;
    let mut chunk = [0u8; 4096];
    loop {
        if expected.is_some_and(|total| bytes.len() >= total) {
            break;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if expected.is_none() {
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&bytes[..end]);
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                expected = Some(end + 4 + length);
            }
        }
    }
    if bytes.is_empty() {
        return None;
    }
    let body_start = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(bytes.len(), |end| end + 4);
    Some(Request {
        raw: String::from_utf8_lossy(&bytes).into_owned(),
        body: bytes[body_start..].to_vec(),
    })
}

/// Writes a complete `Connection: close` response.
pub fn respond(stream: &mut TcpStream, status: &str, extra_headers: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

pub type Handler = dyn Fn(&Request, &mut TcpStream) + Send + Sync;

/// Concurrent fake HTTP server: one thread per connection with a 5 s read timeout
/// (an idle connection cannot block another), and a stop flag + join on drop.
pub struct FakeServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl FakeServer {
    pub fn start(handler: impl Fn(&Request, &mut TcpStream) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake server");
        let addr = listener.local_addr().expect("fake server addr");
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(handler);
        let flag = stop.clone();
        let accept = std::thread::spawn(move || {
            let mut connections = Vec::new();
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                let handler = handler.clone();
                connections.push(std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                    if let Some(request) = read_request(&mut stream) {
                        handler(&request, &mut stream);
                    }
                }));
                connections.retain(|c: &JoinHandle<()>| !c.is_finished());
            }
            for connection in connections {
                resume_join(connection);
            }
        });
        Self {
            addr,
            stop,
            accept: Some(accept),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept so the loop observes the stop flag.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        if let Some(accept) = self.accept.take() {
            resume_join(accept);
        }
    }
}

fn resume_join(handle: JoinHandle<()>) {
    if let Err(panic) = handle.join() {
        std::panic::resume_unwind(panic);
    }
}
