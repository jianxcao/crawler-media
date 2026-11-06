use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use media::{CatalogGet, CatalogRequest, TmdbError};
use ureq::{Body, config::IpFamily, http::Response};

use super::super::{
    TmdbHttp,
    retry::SystemRuntime,
    transport::{Transport, UreqTransport},
};
use super::PATH;

struct LoopbackTransport(String);

impl Transport for LoopbackTransport {
    fn send(
        &self,
        _url: &str,
        _proxy: Option<&str>,
        _family: IpFamily,
        timeout: Duration,
    ) -> Result<Response<Body>, ureq::Error> {
        UreqTransport.send(&self.0, None, IpFamily::Ipv4Only, timeout)
    }
}

enum Reply {
    Redirect,
    Truncated,
    Stalled,
    Success,
}

struct Server {
    url: String,
    stop: mpsc::Sender<()>,
    release: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/movie", listener.local_addr().unwrap());
        let (stop, stopped) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut replies = replies.into_iter();
            let until = Instant::now() + Duration::from_secs(5);
            let mut accepted = 0;
            while accepted < 2 && Instant::now() < until && stopped.try_recv().is_err() {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        accepted += 1;
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut request = [0; 4096];
                        let _ = stream.read(&mut request);
                        let reply = replies.next().unwrap_or(Reply::Success);
                        let bytes: &[u8] = match reply {
                            Reply::Redirect => b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            Reply::Truncated => b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nx",
                            Reply::Stalled => b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\n",
                            Reply::Success => b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nsuccess",
                        };
                        let _ = stream.write_all(bytes);
                        if matches!(reply, Reply::Stalled) {
                            let _ = released.recv_timeout(Duration::from_secs(3));
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            url,
            stop,
            release,
            worker: Some(worker),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.release.send(());
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn client(url: &str) -> (tempfile::TempDir, TmdbHttp) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::Store::open(dir.path().join("data")).unwrap());
    store
        .put_setting(crate::settings_keys::TMDB_API_KEY, "test-key")
        .unwrap();
    let http = TmdbHttp {
        store,
        transport: Arc::new(LoopbackTransport(url.to_owned())),
        runtime: Arc::new(SystemRuntime),
    };
    (dir, http)
}

#[test]
fn redirect_is_terminal_with_real_ureq_transport() {
    let server = Server::start(vec![Reply::Redirect, Reply::Success]);
    let (_dir, http) = client(&server.url);
    let error = http.get(PATH).unwrap_err();
    assert!(matches!(error, TmdbError::Http(ref message) if message.contains("302")));
}

#[test]
fn truncated_real_response_is_retried_as_a_complete_body() {
    let server = Server::start(vec![Reply::Truncated, Reply::Success]);
    let (_dir, http) = client(&server.url);
    assert_eq!(http.get(PATH).unwrap(), "success");
}

#[test]
fn stalled_real_body_obeys_catalog_deadline() {
    let server = Server::start(vec![Reply::Stalled, Reply::Success]);
    let (dir, http) = client(&server.url);
    let (finished, result) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _dir = dir;
        let request = CatalogRequest::new(Instant::now() + Duration::from_millis(300));
        let _ = finished.send(request.scope(|| http.get(PATH)));
    });
    let observed = result.recv_timeout(Duration::from_secs(2));
    let _ = server.release.send(());
    worker.join().unwrap();
    assert!(matches!(observed.unwrap(), Err(TmdbError::Deadline)));
}
