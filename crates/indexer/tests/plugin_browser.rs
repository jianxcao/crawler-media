use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use indexer::{Browser, BrowserConfig, PluginBrowserErrorKind};
use serde_json::{Value, json};
use tungstenite::Message;

struct MockCdp {
    url: String,
    stop: Arc<AtomicBool>,
    commands: Arc<Mutex<Vec<String>>>,
    nav_urls: Arc<Mutex<Vec<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl MockCdp {
    fn start(mode: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        let stop = Arc::new(AtomicBool::new(false));
        let commands = Arc::new(Mutex::new(Vec::new()));
        let nav_urls = Arc::new(Mutex::new(Vec::new()));
        let t_stop = stop.clone();
        let t_cmds = commands.clone();
        let t_navs = nav_urls.clone();
        let thread = thread::spawn(move || {
            let mut workers = Vec::new();
            while !t_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let cmds = t_cmds.clone();
                        let navs = t_navs.clone();
                        workers.push(thread::spawn(move || serve(stream, addr, mode, cmds, navs)));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            }
            for w in workers {
                let _ = w.join();
            }
        });
        Self {
            url,
            stop,
            commands,
            nav_urls,
            thread: Some(thread),
        }
    }
}

impl Drop for MockCdp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve_http_version(mut stream: TcpStream, addr: std::net::SocketAddr, len: usize) {
    let mut drain = vec![0u8; len];
    let _ = stream.read_exact(&mut drain);
    let body = json!({
        "Browser": "Obscura/Mock",
        "webSocketDebuggerUrl": format!("ws://{addr}/devtools/browser/mock")
    })
    .to_string();
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn send_cdp_reply(
    ws: &mut tungstenite::WebSocket<TcpStream>,
    id: u64,
    session_id: Option<&str>,
    result: Value,
) {
    let mut msg = json!({"id": id, "result": result});
    if let Some(sid) = session_id {
        msg["sessionId"] = json!(sid);
    }
    let _ = ws.send(Message::Text(msg.to_string().into()));
}

fn handle_page_navigate(
    ws: &mut tungstenite::WebSocket<TcpStream>,
    id: u64,
    session_id: Option<&str>,
    req: &Value,
    mode: &str,
    ready_state: &mut &str,
    navs: &Arc<Mutex<Vec<String>>>,
) -> bool {
    if let Some(u) = req["params"]["url"].as_str() {
        navs.lock().unwrap().push(u.to_string());
    }
    if mode == "post-nav-disconnect" {
        send_cdp_reply(
            ws,
            id,
            session_id,
            json!({"frameId": "f1", "loaderId": "l1"}),
        );
        let _ = ws.close(None);
        return false;
    }
    if mode == "post-nav-timeout" {
        send_cdp_reply(
            ws,
            id,
            session_id,
            json!({"frameId": "f1", "loaderId": "l1"}),
        );
        return true;
    }
    *ready_state = "complete";
    if mode != "redirect-aborts-initial-loader" {
        let mut ev = json!({
            "method": "Page.lifecycleEvent",
            "params": {"frameId": "f1", "loaderId": "l1", "name": "load"}
        });
        if let Some(sid) = session_id {
            ev["sessionId"] = json!(sid);
        }
        let _ = ws.send(Message::Text(ev.to_string().into()));
    }
    let mut net = json!({
        "method": "Network.responseReceived",
        "params": {
            "requestId": "req-1",
            "frameId": "f1",
            "loaderId": "l1",
            "type": "Document",
            "response": {
                "status": if mode == "iframe-and-base64" { 201 } else { 200 },
                "headers": {"content-type": "text/html", "set-cookie": "leaked=1"}
            }
        }
    });
    if let Some(sid) = session_id {
        net["sessionId"] = json!(sid);
    }
    let _ = ws.send(Message::Text(net.to_string().into()));
    if mode == "iframe-and-base64" {
        let mut child_net = json!({
            "method": "Network.responseReceived",
            "params": {
                "requestId": "req-iframe",
                "frameId": "f-child",
                "loaderId": "l-child",
                "type": "Document",
                "response": {
                    "status": 403,
                    "headers": {"content-type": "text/html"}
                }
            }
        });
        if let Some(sid) = session_id {
            child_net["sessionId"] = json!(sid);
        }
        let _ = ws.send(Message::Text(child_net.to_string().into()));
    }
    if mode == "redirect-and-gbk" || mode == "redirect-aborts-initial-loader" {
        emit_redirect_gbk_doc(ws, session_id);
    }
    send_cdp_reply(
        ws,
        id,
        session_id,
        json!({"frameId": "f1", "loaderId": "l1"}),
    );
    true
}

fn emit_redirect_gbk_doc(ws: &mut tungstenite::WebSocket<TcpStream>, session_id: Option<&str>) {
    let mut r2 = json!({
        "method": "Network.responseReceived",
        "params": {
            "requestId": "req-2",
            "frameId": "f1",
            "loaderId": "l2",
            "type": "Document",
            "response": {
                "status": 202,
                "headers": {
                    "Content-Type": "application/json; charset=gbk",
                    "Content-Encoding": "gzip",
                    "Content-Length": "9999"
                }
            }
        }
    });
    let mut ev2 = json!({
        "method": "Page.lifecycleEvent",
        "params": {"frameId": "f1", "loaderId": "l2", "name": "load"}
    });
    if let Some(sid) = session_id {
        r2["sessionId"] = json!(sid);
        ev2["sessionId"] = json!(sid);
    }
    let _ = ws.send(Message::Text(r2.to_string().into()));
    let _ = ws.send(Message::Text(ev2.to_string().into()));
}

fn handle_cdp_command(
    ws: &mut tungstenite::WebSocket<TcpStream>,
    req: &Value,
    mode: &str,
    ready_state: &mut &str,
    navs: &Arc<Mutex<Vec<String>>>,
) -> bool {
    let id = req["id"].as_u64().unwrap_or(0);
    let method = req["method"].as_str().unwrap_or("");
    let session_id = req["sessionId"].as_str();
    match method {
        "Target.createBrowserContext" => {
            if mode == "incompatible" {
                let msg = json!({"id": id, "error": {"code": -32601, "message": "not found"}});
                let _ = ws.send(Message::Text(msg.to_string().into()));
            } else {
                send_cdp_reply(ws, id, session_id, json!({"browserContextId": "ctx-1"}));
            }
        }
        "Target.createTarget" => {
            if let Some(u) = req["params"]["url"].as_str() {
                navs.lock().unwrap().push(u.to_string());
            }
            send_cdp_reply(ws, id, session_id, json!({"targetId": "tgt-1"}));
        }
        "Target.attachToTarget" => {
            send_cdp_reply(ws, id, session_id, json!({"sessionId": "sess-1"}))
        }
        "Network.enable"
        | "Page.enable"
        | "Page.setLifecycleEventsEnabled"
        | "Network.setCookies"
        | "Target.closeTarget" => send_cdp_reply(ws, id, session_id, json!({})),
        "Target.disposeBrowserContext" => {
            if mode == "slow-dispose" {
                thread::sleep(Duration::from_millis(80));
            }
            if mode == "fail-dispose" {
                let msg = json!({"id": id, "error": {"code": -32000, "message": "dispose failed"}});
                let _ = ws.send(Message::Text(msg.to_string().into()));
            } else {
                send_cdp_reply(ws, id, session_id, json!({}));
            }
        }
        "Page.navigate" => {
            return handle_page_navigate(ws, id, session_id, req, mode, ready_state, navs);
        }
        "Network.getResponseBody" => {
            if mode == "iframe-and-base64" {
                let req_id = req["params"]["requestId"].as_str().unwrap_or("");
                if req_id == "req-1" {
                    send_cdp_reply(
                        ws,
                        id,
                        session_id,
                        json!({"body": "PGgxPk1haW48L2gxPg==", "base64Encoded": true}),
                    );
                } else {
                    send_cdp_reply(
                        ws,
                        id,
                        session_id,
                        json!({"body": "<iframe>wrong-subframe</iframe>", "base64Encoded": false}),
                    );
                }
            } else {
                let msg = json!({"id": id, "error": {"code": -32601, "message": "unsupported"}});
                let _ = ws.send(Message::Text(msg.to_string().into()));
            }
        }
        "Runtime.evaluate" => {
            let expr = req["params"]["expression"].as_str().unwrap_or("");
            let val = if expr.contains("readyState") {
                json!(*ready_state)
            } else if expr.contains("outerHTML") {
                json!("<html><body><div class='row'>OK</div></body></html>")
            } else {
                json!({"ok": true})
            };
            send_cdp_reply(
                ws,
                id,
                session_id,
                json!({"result": {"type": "string", "value": val}}),
            );
        }
        "Network.getCookies" => send_cdp_reply(
            ws,
            id,
            session_id,
            json!({"cookies": [{"name": "sid", "value": "abc", "domain": "site.example", "path": "/"}]}),
        ),
        _ => {
            let msg = json!({"id": id, "error": {"code": -32601, "message": "unknown"}});
            let _ = ws.send(Message::Text(msg.to_string().into()));
        }
    }
    true
}

fn serve(
    stream: TcpStream,
    addr: std::net::SocketAddr,
    mode: &str,
    cmds: Arc<Mutex<Vec<String>>>,
    navs: Arc<Mutex<Vec<String>>>,
) {
    stream.set_nonblocking(false).unwrap();
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut peek = [0u8; 2048];
    let len = loop {
        let n = stream.peek(&mut peek).unwrap_or(0);
        if n == 0 {
            return;
        }
        if peek[..n].windows(4).any(|b| b == b"\r\n\r\n") {
            break n;
        }
    };
    let head = String::from_utf8_lossy(&peek[..len]);
    if head.starts_with("GET /json/version") {
        serve_http_version(stream, addr, len);
        return;
    }

    let mut ws = tungstenite::accept(stream).unwrap();
    let mut ready_state = "loading";
    while let Ok(msg) = ws.read() {
        let Message::Text(text) = msg else {
            continue;
        };
        let req: Value = serde_json::from_str(&text).unwrap();
        if let Some(m) = req["method"].as_str() {
            cmds.lock().unwrap().push(m.to_string());
        }
        if !handle_cdp_command(&mut ws, &req, mode, &mut ready_state, &navs) {
            break;
        }
    }
}

#[test]
fn plugin_browser_isolates_context_and_holds_slot_until_verified_dispose() {
    let cdp = MockCdp::start("slow-dispose");
    let cfg = BrowserConfig {
        enabled: true,
        endpoint: Some(cdp.url.clone()),
        managed_endpoint: None,
    };
    let browser = Arc::new(
        Browser::new(cfg)
            .with_render_deadline(Duration::from_secs(5))
            .with_queue_cap(Duration::from_secs(2))
            .with_min_work(Duration::from_millis(200)),
    );

    let mut lease = browser
        .open_plugin_session(
            "https://site.example/index.php",
            None,
            Instant::now() + Duration::from_secs(4),
        )
        .unwrap();
    let resp = lease
        .navigate("https://site.example/search", "load")
        .unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(
        lease.cookie_header("https://site.example").unwrap(),
        "sid=abc"
    );

    // Second same-domain open must serialize behind the held SlotGuard until verified dispose finishes
    let b2 = browser.clone();
    let started = Instant::now();
    let handle = thread::spawn(move || {
        let mut l2 = b2
            .open_plugin_session(
                "https://sub.site.example/rss",
                None,
                Instant::now() + Duration::from_secs(4),
            )
            .unwrap();
        let elapsed = started.elapsed();
        l2.close_verified().unwrap();
        elapsed
    });

    thread::sleep(Duration::from_millis(60));
    lease.close_verified().unwrap();
    let wait_elapsed = handle.join().unwrap();
    assert!(
        wait_elapsed >= Duration::from_millis(120),
        "second session did not serialize across slow dispose: {wait_elapsed:?}"
    );
    let cmds = cdp.commands.lock().unwrap();
    assert!(cmds.iter().any(|c| c == "Target.createBrowserContext"));
    assert!(cmds.iter().any(|c| c == "Target.closeTarget"));
    assert!(cmds.iter().any(|c| c == "Target.disposeBrowserContext"));
}

#[test]
fn plugin_browser_marks_incompatible_and_reports_post_nav_unknown() {
    let bad = MockCdp::start("incompatible");
    let b1 = Browser::new(BrowserConfig {
        enabled: true,
        endpoint: Some(bad.url.clone()),
        managed_endpoint: None,
    })
    .with_min_work(Duration::from_millis(200));
    let err = b1
        .open_plugin_session(
            "https://site.example/",
            None,
            Instant::now() + Duration::from_secs(3),
        )
        .err()
        .unwrap();
    assert_eq!(err.kind, PluginBrowserErrorKind::Incompatible);
    assert!(b1.incompatible_endpoints().contains_key(&bad.url));

    let disc = MockCdp::start("post-nav-disconnect");
    let b2 = Browser::new(BrowserConfig {
        enabled: true,
        endpoint: Some(disc.url.clone()),
        managed_endpoint: None,
    })
    .with_min_work(Duration::from_millis(200));
    let mut lease = b2
        .open_plugin_session(
            "https://site.example/",
            None,
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap();
    let nav_err = lease
        .navigate("https://site.example/login", "load")
        .unwrap_err();
    assert_eq!(nav_err.kind, PluginBrowserErrorKind::PostNavUnknown);
}

#[test]
fn check_plugin_cdp_context_opens_and_disposes_blank_context_without_site_nav() {
    let cdp = MockCdp::start("ok");
    let browser = Browser::new(BrowserConfig {
        enabled: true,
        endpoint: Some(cdp.url.clone()),
        managed_endpoint: None,
    })
    .with_min_work(Duration::from_millis(200));
    browser
        .check_plugin_cdp_context(None, Instant::now() + Duration::from_secs(3))
        .unwrap();
    let navs = cdp.nav_urls.lock().unwrap();
    assert_eq!(navs.as_slice(), &["about:blank".to_string()]);
    let cmds = cdp.commands.lock().unwrap();
    assert!(cmds.iter().any(|c| c == "Target.disposeBrowserContext"));
    assert!(!cmds.iter().any(|c| c == "Page.navigate"));
}

#[test]
fn check_plugin_cdp_context_records_incompatible_endpoint() {
    let bad = MockCdp::start("incompatible");
    let browser = Browser::new(BrowserConfig {
        enabled: true,
        endpoint: Some(bad.url.clone()),
        managed_endpoint: None,
    });
    let err = browser
        .check_plugin_cdp_context(None, Instant::now() + Duration::from_secs(3))
        .unwrap_err();
    assert_eq!(err.kind, PluginBrowserErrorKind::Incompatible);
    assert!(browser.incompatible_endpoints().contains_key(&bad.url));
}

#[test]
fn navigate_isolates_main_document_from_subframe_and_decodes_base64_body() {
    let cdp = MockCdp::start("iframe-and-base64");
    let browser = Browser::new(BrowserConfig {
        enabled: true,
        endpoint: Some(cdp.url.clone()),
        managed_endpoint: None,
    })
    .with_min_work(Duration::from_millis(200));
    let mut lease = browser
        .open_plugin_session(
            "https://site.example/",
            None,
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap();
    let resp = lease
        .navigate("https://site.example/torrents", "load")
        .unwrap();
    assert_eq!(
        resp.status, 201,
        "child iframe 403 must not overwrite main doc status"
    );
    assert_eq!(
        resp.body, b"<h1>Main</h1>",
        "base64Encoded body must be decoded"
    );
}

#[test]
fn navigate_prefers_latest_main_frame_redirect_and_rewrites_outerhtml_charset_to_utf8() {
    let cdp = MockCdp::start("redirect-and-gbk");
    let browser = Browser::new(BrowserConfig {
        enabled: true,
        endpoint: Some(cdp.url.clone()),
        managed_endpoint: None,
    })
    .with_min_work(Duration::from_millis(200));
    let mut lease = browser
        .open_plugin_session(
            "https://site.example/",
            None,
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap();
    let resp = lease
        .navigate("https://site.example/challenge", "load")
        .unwrap();
    assert_eq!(
        resp.status, 202,
        "client-side redirect on main frame must return latest Document response, not initial l1"
    );
    let ct = resp
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    assert!(
        ct.to_ascii_lowercase().contains("utf-8") && !ct.to_ascii_lowercase().contains("gbk"),
        "outerHTML UTF-8 fallback must rewrite wire charset=gbk to utf-8, got: {ct}"
    );
}

#[test]
fn navigate_redirect_aborts_initial_loader_without_timeout_and_strips_stale_wire_headers() {
    let cdp = MockCdp::start("redirect-aborts-initial-loader");
    let browser = Browser::new(BrowserConfig {
        enabled: true,
        endpoint: Some(cdp.url.clone()),
        managed_endpoint: None,
    })
    .with_min_work(Duration::from_millis(200));
    let mut lease = browser
        .open_plugin_session(
            "https://site.example/",
            None,
            Instant::now() + Duration::from_millis(600),
        )
        .unwrap();
    let resp = lease
        .navigate("https://site.example/challenge", "load")
        .expect("redirect l1->l2 where only l2 fires load must succeed without timeout");
    assert_eq!(resp.status, 202);
    let ct = resp
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    assert_eq!(ct, "text/html; charset=utf-8");
    assert!(
        !resp.headers.iter().any(|(k, _)| {
            k.eq_ignore_ascii_case("content-encoding") || k.eq_ignore_ascii_case("content-length")
        }),
        "outerHTML fallback must strip stale Content-Encoding and Content-Length: {:?}",
        resp.headers
    );
}
