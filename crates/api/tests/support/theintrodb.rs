#![allow(dead_code)]
use std::io::{Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use api::theintrodb::TheIntroDbClient;

type ResponseConfig = (u16, Vec<(String, String)>, String);

pub(crate) struct Server {
    pub(crate) url: String,
    stop: Arc<AtomicBool>,
    response: Arc<std::sync::Mutex<ResponseConfig>>,
    pub(crate) requests: Arc<std::sync::atomic::AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub(crate) fn new(status: u16, body: &str, authenticated: bool) -> Self {
        Self::with_action(status, body, authenticated, || {})
    }

    pub(crate) fn with_headers(
        status: u16,
        headers: &[(&str, &str)],
        body: &str,
        authenticated: bool,
    ) -> Self {
        Self::spawn(
            status,
            headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            body,
            authenticated,
            Some(2700000),
            false,
            || {},
        )
    }

    pub(crate) fn for_any_episode(
        status: u16,
        headers: &[(&str, &str)],
        body: &str,
        authenticated: bool,
    ) -> Self {
        Self::spawn(
            status,
            headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            body,
            authenticated,
            None,
            true,
            || {},
        )
    }

    pub(crate) fn with_action(
        status: u16,
        body: &str,
        authenticated: bool,
        action: impl Fn() + Send + 'static,
    ) -> Self {
        Self::spawn(
            status,
            Vec::new(),
            body,
            authenticated,
            Some(2700000),
            false,
            action,
        )
    }

    pub(crate) fn for_duration(
        status: u16,
        body: &str,
        authenticated: bool,
        duration: Option<i64>,
    ) -> Self {
        Self::spawn(
            status,
            Vec::new(),
            body,
            authenticated,
            duration,
            false,
            || {},
        )
    }

    fn spawn(
        status: u16,
        headers: Vec<(String, String)>,
        body: &str,
        authenticated: bool,
        duration: Option<i64>,
        any_episode: bool,
        action: impl Fn() + Send + 'static,
    ) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let response = Arc::new(std::sync::Mutex::new((status, headers, body.to_string())));
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
                let auth_ok = any_episode
                    || (request.contains("authorization: bearer fixture-key") == authenticated);
                let duration_ok = any_episode
                    || duration.map_or(!request.contains("duration_ms="), |duration| {
                        request.contains(&format!("duration_ms={duration}"))
                    });
                let episode_ok = any_episode || request.contains("episode=2");
                let valid = request.starts_with("get /media?")
                    && request.contains("tmdb_id=123")
                    && request.contains("season=1")
                    && episode_ok
                    && duration_ok
                    && request.contains("details=true")
                    && request.contains("merge_unknown=false")
                    && auth_ok;
                if valid {
                    received.fetch_add(1, Ordering::SeqCst);
                    action();
                }
                let req_episode = extract_query_param(&request, "episode").unwrap_or("2");
                let (status, headers, body) = current_response.lock().unwrap().clone();
                let rendered_body = body.replace("__EP__", req_episode);
                let (status, hdrs, body_str) = if valid {
                    (status, headers, rendered_body)
                } else {
                    (400, Vec::new(), "{}".to_string())
                };
                let mut extra_headers = String::new();
                for (name, value) in &hdrs {
                    extra_headers.push_str(&format!("{name}: {value}\r\n"));
                }
                write!(
                    stream,
                    "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body_str}",
                    body_str.len()
                )
                .unwrap();
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
        self.respond_with_headers(status, &[], body);
    }

    pub(crate) fn respond_with_headers(&self, status: u16, headers: &[(&str, &str)], body: &str) {
        *self.response.lock().unwrap() = (
            status,
            headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            body.to_string(),
        );
    }

    pub(crate) fn client(&self, key: bool) -> TheIntroDbClient {
        TheIntroDbClient::new(key.then(|| "fixture-key".into())).with_base_url(&self.url)
    }
}

fn extract_query_param<'a>(request: &'a str, key: &str) -> Option<&'a str> {
    let path = request.split_whitespace().nth(1)?;
    let (_, query) = path.split_once('?')?;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == key
        {
            return Some(v);
        }
    }
    None
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}
