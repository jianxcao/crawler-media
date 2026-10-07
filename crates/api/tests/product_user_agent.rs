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
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
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
