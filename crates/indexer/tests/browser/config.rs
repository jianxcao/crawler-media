//! Browser configuration and validation: the disabled default, timing and
//! concurrency limits, the websocket origin rule and the group-key parser.

use crate::support::*;

#[test]
fn default_browser_config_is_disabled() {
    let off = BrowserConfig::disabled();
    assert!(!off.enabled);
    assert!(off.endpoint.is_none());
    assert!(off.managed_endpoint.is_none());
}

#[test]
fn timing_constraints_are_validated() {
    use std::time::Duration;
    let b = Browser::new(BrowserConfig::disabled())
        .with_render_deadline(Duration::from_secs(10))
        .with_min_work(Duration::from_secs(10));
    assert!(b.validate_timing().is_err());

    let b2 = Browser::new(BrowserConfig::disabled())
        .with_render_deadline(Duration::from_secs(30))
        .with_min_work(Duration::from_secs(10))
        .with_queue_cap(Duration::from_secs(25));
    assert!(b2.validate_timing().is_err());

    let valid = Browser::new(BrowserConfig::disabled());
    assert!(valid.validate_timing().is_ok());
}

#[test]
fn websocket_url_on_a_different_host_or_port_is_rejected() {
    // Discovery endpoint is 127.0.0.1:9222, but returns ws://10.0.0.1:9222
    let fixture = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop.clone();
        let handle = std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut buf = [0; 1024];
                    let _ = stream.read(&mut buf);
                    let body = serde_json::json!({
                        "id": "target1",
                        "webSocketDebuggerUrl": "ws://10.0.0.9:9999/devtools/page/x"
                    })
                    .to_string();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        (format!("http://{addr}"), stop, handle)
    };

    let res = indexer::cdp_page::open_cdp_session(&fixture.0);
    fixture.1.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = fixture.2.join();
    let err = match res {
        Err(e) => e,
        Ok(_) => panic!("expected mismatched debugger url error"),
    };
    assert!(err.to_string().contains("mismatched debugger url"));
}

#[test]
fn try_group_key_errors_on_unnormalizable_host() {
    assert!(indexer::browser_host::try_group_key("").is_err());
    assert_eq!(
        indexer::browser_host::try_group_key("a.example.org").unwrap(),
        "example.org"
    );
}
