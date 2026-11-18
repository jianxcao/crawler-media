#![allow(dead_code)]
use std::io::{Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use api::theintrodb::TheIntroDbClient;

pub(crate) struct Server {
    pub(crate) url: String,
    stop: Arc<AtomicBool>,
    response: Arc<std::sync::Mutex<(u16, String)>>,
    pub(crate) requests: Arc<std::sync::atomic::AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub(crate) fn new(status: u16, body: &str, authenticated: bool) -> Self {
        Self::with_action(status, body, authenticated, || {})
    }

    pub(crate) fn with_action(
        status: u16,
        body: &str,
        authenticated: bool,
        action: impl Fn() + Send + 'static,
    ) -> Self {
        Self::with_duration(status, body, authenticated, Some(2700000), action)
    }

    pub(crate) fn for_duration(
        status: u16,
        body: &str,
        authenticated: bool,
        duration: Option<i64>,
    ) -> Self {
        Self::with_duration(status, body, authenticated, duration, || {})
    }

    fn with_duration(
        status: u16,
        body: &str,
        authenticated: bool,
        duration: Option<i64>,
        action: impl Fn() + Send + 'static,
    ) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let response = Arc::new(std::sync::Mutex::new((status, body.to_string())));
        let current_response = response.clone();
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let received = requests.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("HTTP fixture failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = [0; 8192];
                let count = stream.read(&mut bytes).unwrap();
                let request = String::from_utf8_lossy(&bytes[..count]).to_ascii_lowercase();
                let valid = request.starts_with("get /media?")
                    && request.contains("tmdb_id=123")
                    && request.contains("season=1")
                    && request.contains("episode=2")
                    && duration.map_or(!request.contains("duration_ms="), |duration| {
                        request.contains(&format!("duration_ms={duration}"))
                    })
                    && request.contains("details=true")
                    && request.contains("merge_unknown=false")
                    && (request.contains("authorization: bearer fixture-key") == authenticated);
                if valid {
                    received.fetch_add(1, Ordering::SeqCst);
                    action();
                }
                let (status, body) = current_response.lock().unwrap().clone();
                let (status, body) = if valid {
                    (status, body.as_str())
                } else {
                    (400, "{}")
                };
                write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        Self {
            url,
            stop,
            response,
            requests,
            thread: Some(thread),
        }
    }

    pub(crate) fn respond(&self, status: u16, body: &str) {
        *self.response.lock().unwrap() = (status, body.to_string());
    }

    pub(crate) fn client(&self, key: bool) -> TheIntroDbClient {
        TheIntroDbClient::new(key.then(|| "fixture-key".into())).with_base_url(&self.url)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}
