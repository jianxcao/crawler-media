//! C18: test fixtures release gates and stop fake servers on every exit path.
#[path = "support/lifecycle.rs"]
mod lifecycle;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lifecycle::{FakeServer, Gate, ReleaseOnDrop, respond};

const CHILD_ENV: &str = "FIXTURE_LIFECYCLE_PANIC_CHILD";

/// Run only as the child process: a blocking task waits on the gate, the test
/// panics before releasing it. The runtime's shutdown waits for blocking tasks, so
/// without RAII release the child would sit out the gate's whole wait.
#[test]
fn panicking_child_body() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let gate = Arc::new(Gate::default());
        let _release = ReleaseOnDrop(gate.clone());
        let waiter = gate.clone();
        tokio::task::spawn_blocking(move || waiter.wait(Duration::from_secs(60)));
        tokio::time::sleep(Duration::from_millis(50)).await;
        panic!("intentional failure before gate.release()");
    });
}

#[test]
fn panicking_test_releases_its_gate_and_exits() {
    let exe = std::env::current_exe().unwrap();
    let started = Instant::now();
    let mut child = std::process::Command::new(exe)
        .args([
            "--exact",
            "panicking_child_body",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = started + Duration::from_secs(5);
    let mut panicking_child_exited_before_deadline = false;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(!status.success(), "the child test must fail");
            panicking_child_exited_before_deadline = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !panicking_child_exited_before_deadline {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert!(panicking_child_exited_before_deadline);
}

fn status_of(response: &str) -> u16 {
    response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

#[test]
fn idle_connection_does_not_block_others_and_drop_joins_the_server() {
    let server = FakeServer::start(|_request, stream| respond(stream, "200 OK", "", b"ok"));
    let addr = server.addr();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // An idle connection is opened first and never sends a byte.
        let _idle = TcpStream::connect(addr).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let mut second = TcpStream::connect(addr).unwrap();
        second
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        second
            .write_all(b"GET /ping HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        let _ = second.read_to_string(&mut response);
        let _ = done_tx.send(response);
    });
    let second_connection_response = done_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("second connection answered while the first stays idle");
    assert_eq!(status_of(&second_connection_response), 200);
    let (joined_tx, joined_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(server);
        let _ = joined_tx.send(());
    });
    let server_thread_joined = joined_rx.recv_timeout(Duration::from_secs(10)).is_ok();
    assert!(server_thread_joined);
}

#[test]
fn handler_panic_surfaces_when_the_server_stops() {
    let server = FakeServer::start(|_, _| {
        std::thread::sleep(Duration::from_millis(200));
        panic!("handler blew up");
    });
    let mut stream = TcpStream::connect(server.addr()).unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(server)));
    assert!(result.is_err(), "a handler panic must not disappear inside join");
}
