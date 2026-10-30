//! Exercise the actual HTTP adapter against a finite local origin.
use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    time::{Duration, Instant},
};

fn origin(status: u16) -> (String, mpsc::Receiver<String>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (send, request) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "local origin was not contacted");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 1024];
        while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0, "request ended before headers");
            bytes.extend_from_slice(&chunk[..count]);
        }
        send.send(String::from_utf8(bytes).unwrap()).unwrap();
        let body = r#"{"id":"42","title":"Finite subject"}"#;
        let response = format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
    });
    (format!("http://{address}"), request, worker)
}

#[test]
fn rexxar_transport_keeps_mobile_headers_and_complete_body() {
    let (base, request, worker) = origin(200);
    let body = DoubanHttp
        .get_with_origin("/rexxar/api/v2/movie/42?for_mobile=1", Some(&base))
        .unwrap();
    let headers = request
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .to_lowercase();
    worker.join().unwrap();
    assert!(headers.starts_with("get /rexxar/api/v2/movie/42?for_mobile=1 http/1.1\r\n"));
    assert!(headers.contains("user-agent: mozilla/5.0 (iphone; cpu iphone os 17_0 like mac os x) applewebkit/605.1.15\r\n"), "{headers}");
    assert!(
        headers.contains("referer: https://m.douban.com/movie/\r\n"),
        "{headers}"
    );
    let data: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(data["title"], "Finite subject");
}

#[test]
fn rexxar_transport_404_is_typed_missing() {
    let (base, request, worker) = origin(404);
    let result =
        DoubanHttp.get_with_origin("/rexxar/api/v2/movie/missing?for_mobile=1", Some(&base));
    request.recv_timeout(Duration::from_secs(2)).unwrap();
    worker.join().unwrap();
    assert!(matches!(result, Err(TmdbError::NotFound(_))), "{result:?}");
}

#[test]
fn transport_transient_failure_stays_distinct_from_missing() {
    let (base, request, worker) = origin(503);
    let result = DoubanHttp.get_with_origin("/subject/42/", Some(&base));
    let headers = request
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .to_lowercase();
    worker.join().unwrap();
    assert!(matches!(result, Err(TmdbError::Http(_))), "{result:?}");
    assert!(headers.contains("referer: https://movie.douban.com/\r\n"));
    assert!(headers.contains("user-agent: mozilla/5.0 (macintosh;"));
}
