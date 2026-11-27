use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use net_guard::{DnsResolver, NetGuardError};

#[derive(Debug)]
pub struct FakeResolver {
    records: Mutex<HashMap<String, Vec<Vec<IpAddr>>>>,
    calls: AtomicUsize,
}

impl FakeResolver {
    pub fn new(map: HashMap<String, Vec<Vec<IpAddr>>>) -> Self {
        Self {
            records: Mutex::new(map),
            calls: AtomicUsize::new(0),
        }
    }

    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl DnsResolver for FakeResolver {
    fn resolve(
        &self,
        host: &str,
        _port: u16,
        _timeout: Duration,
    ) -> Result<Vec<IpAddr>, NetGuardError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut records = self.records.lock().unwrap();
        let queue = records
            .get_mut(host)
            .ok_or_else(|| NetGuardError::Transport(format!("no fake dns for {host}")))?;
        if queue.len() > 1 {
            Ok(queue.remove(0))
        } else {
            Ok(queue.first().cloned().unwrap_or_default())
        }
    }
}

fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut header_end = None;
    let mut content_len = 0usize;
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if header_end.is_none()
                    && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                {
                    let end = pos + 4;
                    header_end = Some(end);
                    let headers = String::from_utf8_lossy(&buf[..end]);
                    for line in headers.lines() {
                        if let Some((k, v)) = line.split_once(':')
                            && k.trim().eq_ignore_ascii_case("content-length")
                        {
                            content_len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                if let Some(end) = header_end
                    && buf.len() >= end + content_len
                {
                    break;
                }
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

pub struct TestServer {
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    captured: Arc<Mutex<Vec<String>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    pub fn spawn(responses: Vec<Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            for resp in responses {
                loop {
                    if stop_flag.load(Ordering::SeqCst) || Instant::now() > deadline {
                        return;
                    }
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            let req = read_http_request(&mut stream);
                            captured_clone.lock().unwrap().push(req);
                            let _ = stream.write_all(&resp);
                            let _ = stream.flush();
                            break;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => return,
                    }
                }
            }
        });
        Self {
            addr,
            stop,
            captured,
            handle: Some(handle),
        }
    }

    pub fn take_requests(&mut self) -> Vec<String> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.captured.lock().unwrap().clone()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
