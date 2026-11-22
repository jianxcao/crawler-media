use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use api::Store;

#[test]
fn saved_product_user_agent_is_sent_by_catalog_agent() {
    let tmp = tempfile::tempdir().unwrap();
    api::user_agent::sync_user_agent(&Store::open(tmp.path().join("defaults")).unwrap());
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .put_setting(
            api::settings_keys::GLOBAL_USER_AGENT,
            "crawler-media-test/42",
        )
        .unwrap();
    api::user_agent::sync_user_agent(&store);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "product user-agent request did not arrive within deadline"
                    );
                    thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(err) => panic!("accept failed: {err}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        socket
            .set_write_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut buffer = [0_u8; 4096];
        let count = socket.read(&mut buffer).unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        String::from_utf8_lossy(&buffer[..count]).into_owned()
    });

    api::http_agent::call_with_proxy(None, |agent| {
        agent.get(&format!("http://{address}/test")).call()
    })
    .unwrap();
    let request = server.join().unwrap();
    assert!(
        request.contains("user-agent: crawler-media-test/42\r\n"),
        "{request}"
    );
    api::user_agent::sync_user_agent(&Store::open(tmp.path().join("other")).unwrap());
}
