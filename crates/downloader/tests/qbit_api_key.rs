use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

use domain::{SiteId, Torrent};
use downloader::{Downloader, QbitConfig, QbitDownloader};
use parking_lot::Mutex;

#[derive(Default)]
struct Capture {
    requests: Mutex<Vec<RecordedRequest>>,
    added: std::sync::atomic::AtomicBool,
    fail_info_with: Mutex<Option<u16>>,
    fail_version_with: Mutex<Option<u16>>,
}

#[derive(Clone, Debug)]
struct RecordedRequest {
    path: String,
    auth: Option<String>,
    cookie: Option<String>,
}

struct FakeServer {
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn spawn_qbit_api_key_stub(capture: Arc<Capture>) -> (String, FakeServer) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = shutdown.clone();
    let handle = thread::spawn(move || {
        while !flag.load(std::sync::atomic::Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let cap = capture.clone();
                    thread::spawn(move || handle_conn(&mut stream, &cap));
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    (
        format!("http://{addr}"),
        FakeServer {
            shutdown,
            handle: Some(handle),
        },
    )
}

fn handle_conn(stream: &mut std::net::TcpStream, capture: &Capture) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            }
            Err(_) => return,
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = head.split("\r\n");
    let req_line = lines.next().unwrap_or("");
    let path = req_line.split_whitespace().nth(1).unwrap_or("").to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let content_len = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < header_end + content_len {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    capture.requests.lock().push(RecordedRequest {
        path: path.clone(),
        auth: headers.get("authorization").cloned(),
        cookie: headers.get("cookie").cloned(),
    });

    let authorized =
        headers.get("authorization").map(String::as_str) == Some("Bearer qbt_secret_key");
    let (status, body) = if path.starts_with("/api/v2/auth/login") {
        (
            "500 Internal Server Error",
            "login must not be called".to_string(),
        )
    } else if path.starts_with("/api/v2/app/version") {
        if let Some(code) = *capture.fail_version_with.lock() {
            (status_line(code), "Unauthorized".to_string())
        } else if authorized {
            ("200 OK", "v5.3.0".to_string())
        } else {
            ("401 Unauthorized", "Invalid API key".to_string())
        }
    } else if !authorized {
        ("401 Unauthorized", "Invalid API key".to_string())
    } else if path.starts_with("/api/v2/torrents/add") {
        capture
            .added
            .store(true, std::sync::atomic::Ordering::Relaxed);
        ("200 OK", "Ok.".to_string())
    } else if path.starts_with("/api/v2/torrents/info") {
        if let Some(code) = *capture.fail_info_with.lock() {
            (status_line(code), "Forbidden".to_string())
        } else if capture.added.load(std::sync::atomic::Ordering::Relaxed) {
            (
                "200 OK",
                r#"[{"hash":"0123456789abcdef0123456789abcdef01234567","name":"Movie.2024","state":"uploading","progress":1.0,"save_path":"/downloads","content_path":"/downloads/Movie.2024.mkv","tags":"crawler-media,crawler-media-owned-0011223344556677","category":"crawler-media","total_size":1024,"dlspeed":0,"upspeed":0}]"#
                    .to_string(),
            )
        } else {
            ("200 OK", "[]".to_string())
        }
    } else if path.starts_with("/api/v2/torrents/files") {
        (
            "200 OK",
            r#"[{"index":0,"name":"Movie.2024.mkv","progress":1.0,"priority":1}]"#.to_string(),
        )
    } else if path.starts_with("/api/v2/app/preferences") {
        (
            "200 OK",
            r#"{"save_path":"/downloads","dl_limit":0,"up_limit":0}"#.to_string(),
        )
    } else if path.starts_with("/api/v2/torrents/delete") {
        ("200 OK", String::new())
    } else {
        ("404 Not Found", "not found".to_string())
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn status_line(code: u16) -> &'static str {
    match code {
        401 => "401 Unauthorized",
        403 => "403 Forbidden",
        _ => "500 Internal Server Error",
    }
}

fn sample_torrent() -> Torrent {
    Torrent {
        site_id: SiteId::new(),
        title: "Movie.2024".into(),
        enclosure: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
        size_bytes: Some(1024),
        seeders: Some(10),
        free: false,
        hr: None,
        imdb_id: None,
        id: None,
        leechers: Some(1),
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    }
}

#[test]
fn api_key_connects_via_bearer_header_without_login_and_sends_bearer_on_all_calls() {
    let capture = Arc::new(Capture::default());
    let (url, _srv) = spawn_qbit_api_key_stub(capture.clone());
    let dl = QbitDownloader::connect(QbitConfig {
        url,
        username: String::new(),
        password: String::new(),
        api_key: Some("qbt_secret_key".into()),
        category: Some("crawler-media".into()),
        path_maps: Vec::new(),
    })
    .expect("Bearer API key should connect without /auth/login");

    dl.add(&sample_torrent()).unwrap();
    let _ = dl.completed_files(&sample_torrent()).unwrap();
    let _ = dl.get_speed_limits().unwrap();
    dl.delete_task("0123456789abcdef0123456789abcdef01234567", true)
        .unwrap();

    let reqs = capture.requests.lock();
    assert!(
        !reqs
            .iter()
            .any(|r| r.path.starts_with("/api/v2/auth/login")),
        "API key mode must never hit /api/v2/auth/login: {reqs:?}"
    );
    for expected in [
        "/api/v2/app/version",
        "/api/v2/torrents/add",
        "/api/v2/torrents/info",
        "/api/v2/app/preferences",
        "/api/v2/torrents/delete",
    ] {
        let matching: Vec<_> = reqs
            .iter()
            .filter(|r| r.path.starts_with(expected))
            .collect();
        assert!(
            !matching.is_empty(),
            "expected call to {expected}, got {reqs:?}"
        );
        for req in matching {
            assert_eq!(
                req.auth.as_deref(),
                Some("Bearer qbt_secret_key"),
                "{req:?}"
            );
            assert_eq!(
                req.cookie, None,
                "must not send Cookie in API key mode: {req:?}"
            );
        }
    }
}

#[test]
fn invalid_api_key_401_or_403_fails_immediately_without_cookie_relogin() {
    let capture = Arc::new(Capture::default());
    *capture.fail_version_with.lock() = Some(401);
    let (url, _srv) = spawn_qbit_api_key_stub(capture.clone());

    let err = match QbitDownloader::connect(QbitConfig {
        url: url.clone(),
        username: String::new(),
        password: String::new(),
        api_key: Some("qbt_wrong".into()),
        category: None,
        path_maps: Vec::new(),
    }) {
        Ok(_) => panic!("expected 401 failure"),
        Err(err) => err,
    };
    let msg = err.to_string();
    assert!(msg.contains("401") || msg.contains("API Key"), "got: {msg}");

    *capture.fail_version_with.lock() = None;
    let dl = QbitDownloader::connect(QbitConfig {
        url,
        username: String::new(),
        password: String::new(),
        api_key: Some("qbt_secret_key".into()),
        category: None,
        path_maps: Vec::new(),
    })
    .unwrap();
    *capture.fail_info_with.lock() = Some(403);
    let err = dl.completed_files(&sample_torrent()).unwrap_err();
    assert!(err.to_string().contains("403"), "got: {err}");
    assert!(
        !capture
            .requests
            .lock()
            .iter()
            .any(|r| r.path.starts_with("/api/v2/auth/login")),
        "403 in API key mode must never attempt cookie relogin"
    );
}
