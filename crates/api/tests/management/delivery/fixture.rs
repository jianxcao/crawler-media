use std::sync::Arc;

use parking_lot::Mutex;

pub(super) fn transmission_fixture() -> (String, Arc<Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/transmission/rpc", listener.local_addr().unwrap());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut bytes = Vec::new();
            let mut expected = None;
            loop {
                let mut buffer = [0; 4096];
                let Ok(size) = stream.read(&mut buffer) else {
                    break;
                };
                if size == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..size]);
                if expected.is_none() {
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]);
                        let length = header
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
                if expected.is_some_and(|total| bytes.len() >= total) {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&bytes).to_string();
            recorded.lock().push(request.clone());
            let body = if request.contains("torrent-get") {
                r#"{"result":"success","arguments":{"torrents":[{"id":1,"name":"The.Matrix.1999.1080p","hashString":"0123456789abcdef0123456789abcdef01234567","sizeWhenDone":100,"labels":["crawler-media"]}]}}"#
            } else {
                r#"{"result":"success","arguments":{"rpc-version":16}}"#
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nX-Transmission-Session-Id: test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (url, calls)
}
