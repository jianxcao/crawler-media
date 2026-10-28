use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{Receiver, Sender};

struct LoginServer {
    url: String,
    entered: Receiver<()>,
    release: Option<Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl LoginServer {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "verification did not connect");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            // BSD/macOS may inherit O_NONBLOCK from the listening socket.
            // A read timeout only bounds blocking reads; clear the flag explicitly.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 2048];
            while !request.windows(4).any(|chunk| chunk == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
            }
            let header_end = request
                .windows(4)
                .position(|chunk| chunk == b"\r\n\r\n")
                .unwrap()
                + 4;
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let body_len: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap_or(0);
            while request.len() < header_end + body_len {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
            }
            assert!(String::from_utf8_lossy(&request).starts_with("POST /api/v2/auth/login "));
            entered_tx.send(()).unwrap();
            let _ = released.recv_timeout(Duration::from_secs(10));
            stream.write_all(b"HTTP/1.1 200 OK\r\nSet-Cookie: SID=offline\r\nContent-Length: 3\r\nConnection: close\r\n\r\nOk.").unwrap();
        });
        Self {
            url,
            entered,
            release: Some(release),
            thread: Some(thread),
        }
    }
    fn wait(&self) {
        self.entered.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    fn release(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}
impl Drop for LoginServer {
    fn drop(&mut self) {
        self.release();
        if let Some(thread) = self.thread.take() {
            if let Err(error) = thread.join() {
                if !std::thread::panicking() {
                    std::panic::resume_unwind(error);
                }
            }
        }
    }
}

#[test]
fn verification_is_offlock_and_does_not_publish_after_config_or_admission_changes() {
    for change in 0..4 {
        let f = Fixture::new();
        let mut server = LoginServer::new();
        {
            let handle = f.state.store();
            let store = handle.lock();
            let mut row = store.get_downloader(f.downloader).unwrap().unwrap();
            row.kind = "qbittorrent".into();
            row.url = server.url.clone();
            store.save_downloader(&row).unwrap();
        }
        let app = router(f.state.clone());
        let uri = f.downloader_uri("/verify");
        let response = std::thread::spawn(move || {
            runtime()
                .block_on(app.oneshot(request("POST", &uri, Value::Null)))
                .unwrap()
        });
        server.wait();
        {
            let handle = f.state.store();
            let store = handle
                .try_lock()
                .expect("network verification must not hold Store");
            if change == 1 {
                let mut row = store.get_downloader(f.downloader).unwrap().unwrap();
                row.url = "https://changed.invalid/".into();
                store.save_downloader(&row).unwrap();
            }
        }
        let gate = f.state.write_gate();
        let closed = if change == 3 {
            Some(gate.close())
        } else {
            None
        };
        if change == 2 {
            let _ = f.state.clone().with_instance_writable(false);
        }
        server.release();
        let response = response.join().unwrap();
        assert_eq!(
            response.status().as_u16(),
            match change {
                0 => 200,
                1 | 2 => 409,
                _ => 503,
            }
        );
        let verified = f
            .state
            .store()
            .lock()
            .get_setting(&format!("downloader.{}.verified", f.downloader))
            .unwrap();
        assert_eq!(
            verified.as_deref(),
            if change == 0 { Some("true") } else { None }
        );
        drop(closed);
    }
}

#[test]
fn verification_status_is_atomic_when_error_cleanup_fails() {
    let f = Fixture::new();
    let mut server = LoginServer::new();
    let verified_key = format!("downloader.{}.verified", f.downloader);
    let error_key = format!("downloader.{}.last_error", f.downloader);
    {
        let handle = f.state.store();
        let store = handle.lock();
        let mut row = store.get_downloader(f.downloader).unwrap().unwrap();
        row.kind = "qbittorrent".into();
        row.url = server.url.clone();
        store.save_downloader(&row).unwrap();
        store.put_setting(&verified_key, "false").unwrap();
        store.put_setting(&error_key, "prior error").unwrap();
    }
    let conn = rusqlite::Connection::open(f._tmp.path().join("data/app.db")).unwrap();
    conn.execute_batch(&format!("CREATE TRIGGER reject_verification_cleanup BEFORE DELETE ON settings WHEN OLD.key='{error_key}' BEGIN SELECT RAISE(ABORT,'injected verification cleanup'); END;")).unwrap();
    let app = router(f.state.clone());
    let uri = f.downloader_uri("/verify");
    let response = std::thread::spawn(move || {
        runtime()
            .block_on(app.oneshot(request("POST", &uri, Value::Null)))
            .unwrap()
    });
    server.wait();
    server.release();
    let response = response.join().unwrap();
    assert_eq!(response.status().as_u16(), 500);
    let handle = f.state.store();
    let store = handle.lock();
    assert_eq!(
        store.get_setting(&verified_key).unwrap().as_deref(),
        Some("false")
    );
    assert_eq!(
        store.get_setting(&error_key).unwrap().as_deref(),
        Some("prior error")
    );
}
