//! Test-only lifecycle helpers shared by api and downloader tests (`#[path]`
//! included): a gate released on every exit path, and a fake HTTP server that
//! serves connections concurrently and stops and joins on drop.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
const IO_POLL: Duration = Duration::from_millis(200);

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
    let deadline = Instant::now() + REQUEST_DEADLINE;
    let mut bytes = Vec::new();
    let mut expected = None;
    let mut chunk = [0u8; 4096];
    loop {
        if expected.is_some_and(|total| bytes.len() >= total) {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        stream.set_read_timeout(Some(remaining.min(IO_POLL))).ok()?;
        let n = match stream.read(&mut chunk) {
            Ok(n) => n,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => return None,
        };
        if n == 0 {
            return None;
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
                expected = Some((end + 4).checked_add(length)?);
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

/// Concurrent fake HTTP server. Socket IO has a total 5 s deadline; dropping the
/// server closes every active socket and joins every handler before returning.
pub struct FakeServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl FakeServer {
    pub fn start(handler: impl Fn(&Request, &mut TcpStream) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake server");
        listener
            .set_nonblocking(true)
            .expect("nonblocking fake listener");
        let addr = listener.local_addr().expect("fake server addr");
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(handler);
        let flag = stop.clone();
        let accept = std::thread::spawn(move || serve(listener, flag, handler));
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
        if let Some(accept) = self.accept.take() {
            accept.thread().unpark();
            resume_join(accept);
        }
    }
}

fn resume_join(handle: JoinHandle<()>) {
    if let Err(panic) = handle.join() {
        if !std::thread::panicking() {
            std::panic::resume_unwind(panic);
        }
        // The handler's panic hook has already reported this failure. Keep the
        // test's original panic instead of aborting its process with a double panic.
    }
}

type Panic = Box<dyn std::any::Any + Send + 'static>;

struct Connection {
    socket: TcpStream,
    handler: JoinHandle<()>,
    deadline: Instant,
}

impl Connection {
    fn start(mut stream: TcpStream, stop: Arc<AtomicBool>, handler: Arc<Handler>) -> Self {
        let socket = stream
            .try_clone()
            .expect("clone fake connection for shutdown");
        let deadline = Instant::now() + REQUEST_DEADLINE;
        let handler = std::thread::spawn(move || {
            stream
                .set_write_timeout(Some(IO_POLL))
                .expect("fake write timeout");
            if let Some(request) = read_request(&mut stream)
                && !stop.load(Ordering::SeqCst)
            {
                handler(&request, &mut stream);
            }
        });
        Self {
            socket,
            handler,
            deadline,
        }
    }
}

fn serve(listener: TcpListener, stop: Arc<AtomicBool>, handler: Arc<Handler>) {
    let mut connections = Vec::<Connection>::new();
    let mut panicked = None;
    while !stop.load(Ordering::SeqCst) {
        reap_connections(&mut connections, &mut panicked);
        match listener.accept() {
            Ok((stream, _)) => {
                connections.push(Connection::start(stream, stop.clone(), handler.clone()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::park_timeout(Duration::from_millis(10));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                panicked.get_or_insert_with(|| Box::new(format!("fake accept failed: {error}")));
                break;
            }
        }
    }
    // Unblock all reads and writes before waiting for any individual handler.
    for connection in &connections {
        let _ = connection.socket.shutdown(Shutdown::Both);
    }
    for connection in connections {
        join_connection(connection, &mut panicked);
    }
    if let Some(panic) = panicked {
        std::panic::resume_unwind(panic);
    }
}

fn reap_connections(connections: &mut Vec<Connection>, panicked: &mut Option<Panic>) {
    let mut index = 0;
    while index < connections.len() {
        let connection = &connections[index];
        if Instant::now() >= connection.deadline {
            let _ = connection.socket.shutdown(Shutdown::Both);
        }
        if connection.handler.is_finished() {
            join_connection(connections.swap_remove(index), panicked);
        } else {
            index += 1;
        }
    }
}

fn join_connection(connection: Connection, panicked: &mut Option<Panic>) {
    if let Err(panic) = connection.handler.join() {
        panicked.get_or_insert(panic);
    }
}
