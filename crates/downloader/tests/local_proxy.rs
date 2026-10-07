use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use downloader::{QbitConfig, QbitDownloader, TransmissionConfig, TransmissionDownloader};

const CHILD_ENV: &str = "CRAWLER_MEDIA_LOCAL_PROXY_TEST_CHILD";
const TEST_NAME: &str = "loopback_downloader_connections_bypass_system_proxy";

struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}

impl Server {
    fn once(status: &'static str, headers: &'static str, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = read_request(&mut stream);
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            url: format!("http://{addr}"),
            stop,
            worker,
        }
    }

    fn close(self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.join().unwrap();
    }
}

struct ProxyServer {
    url: String,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}

impl ProxyServer {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let worker_hits = hits.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        worker_hits.fetch_add(1, Ordering::Relaxed);
                        let _ = read_request(&mut stream);
                        let _ = stream.write_all(
                            b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            url: format!("http://{addr}"),
            hits,
            stop,
            worker,
        }
    }

    fn close(self) -> usize {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.join().unwrap();
        self.hits.load(Ordering::Relaxed)
    }
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut expected = None;
    loop {
        let mut buffer = [0; 1024];
        let Ok(read) = stream.read(&mut buffer) else {
            break;
        };
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if expected.is_none() {
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]);
                let length = headers.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                });
                expected = Some(end + 4 + length.unwrap_or(0));
            }
        }
        if expected.is_some_and(|length| request.len() >= length) {
            break;
        }
    }
    request
}

#[test]
fn loopback_downloader_connections_bypass_system_proxy() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let qb = Server::once("200 OK", "Set-Cookie: SID=test; HttpOnly\r\n", "Ok.");
        let tr = Server::once(
            "200 OK",
            "Content-Type: application/json\r\n",
            r#"{"result":"success","arguments":{"rpc-version":16}}"#,
        );
        let qb_result = QbitDownloader::connect(QbitConfig {
            url: qb.url.clone(),
            username: "admin".into(),
            password: "pass".into(),
            category: None,
            path_maps: vec![],
        });
        assert!(
            qb_result.is_ok(),
            "loopback qBittorrent connection: {:?}",
            qb_result.as_ref().err()
        );
        let tr_result = TransmissionDownloader::connect(TransmissionConfig {
            url: tr.url.clone(),
            username: None,
            password: None,
            path_maps: vec![],
        });
        assert!(
            tr_result.is_ok(),
            "loopback Transmission connection: {:?}",
            tr_result.as_ref().err()
        );
        qb.close();
        tr.close();
        return;
    }

    let proxy = ProxyServer::new();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_ENV, "1")
        .env("HTTP_PROXY", &proxy.url)
        .env("HTTPS_PROXY", &proxy.url)
        .env("ALL_PROXY", &proxy.url)
        .env("http_proxy", &proxy.url)
        .env("https_proxy", &proxy.url)
        .env("all_proxy", &proxy.url)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .output()
        .unwrap();
    let proxy_hits = proxy.close();

    assert!(
        output.status.success(),
        "local downloader connection failed with a system proxy set:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        proxy_hits, 0,
        "loopback downloader traffic reached the proxy"
    );
}
