use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

use crate::lifecycle::{FakeServer, respond};

fn trickle(stream: &TcpStream) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let mut stream = stream.try_clone().unwrap();
    let writer = std::thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            if stream.write_all(b"x").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    });
    (stop, writer)
}

#[test]
fn dropping_server_cancels_a_trickling_request() {
    let server = FakeServer::start(|_, _| {});
    let mut stream = TcpStream::connect(server.addr()).unwrap();
    stream.write_all(b"GET / HTTP/1.1\r\nX-Slow: ").unwrap();
    let (stop, writer) = trickle(&stream);
    std::thread::sleep(Duration::from_millis(75));
    let (done_tx, done_rx) = mpsc::channel();
    let dropper = std::thread::spawn(move || {
        drop(server);
        done_tx.send(()).unwrap();
    });
    let stopped = done_rx.recv_timeout(Duration::from_millis(750)).is_ok();
    stop.store(true, Ordering::SeqCst);
    let _ = stream.shutdown(Shutdown::Both);
    writer.join().unwrap();
    if !stopped {
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    dropper.join().unwrap();
    assert!(
        stopped,
        "shutdown must close active sockets despite incoming bytes"
    );
}

#[test]
fn request_deadline_expires_despite_continuous_header_progress() {
    let called = Arc::new(AtomicBool::new(false));
    let observed = called.clone();
    let server = FakeServer::start(move |_, _| {
        observed.store(true, Ordering::SeqCst);
    });
    let mut stream = TcpStream::connect(server.addr()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(6)))
        .unwrap();
    stream.write_all(b"GET / HTTP/1.1\r\nX-Slow: ").unwrap();
    let (stop, writer) = trickle(&stream);
    let closed = matches!(stream.read(&mut [0]), Ok(0));
    stop.store(true, Ordering::SeqCst);
    let _ = stream.shutdown(Shutdown::Both);
    writer.join().unwrap();
    drop(server);
    assert!(
        closed,
        "continuous partial headers must not bypass the request deadline"
    );
    assert!(
        !called.load(Ordering::SeqCst),
        "incomplete requests must never reach handlers"
    );
}

#[test]
fn completed_handler_panic_is_reported_after_a_later_connection() {
    let server = FakeServer::start(|request, stream| {
        if request.raw.starts_with("GET /panic ") {
            panic!("completed handler failed");
        }
        respond(stream, "200 OK", "", b"ok");
    });
    let mut failed = TcpStream::connect(server.addr()).unwrap();
    failed
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    failed
        .write_all(b"GET /panic HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    assert_eq!(failed.read(&mut [0]).unwrap(), 0);
    let mut second = TcpStream::connect(server.addr()).unwrap();
    second
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    second
        .write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    second.read_to_string(&mut response).unwrap();
    assert!(response.contains("200 OK"));
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(server))).is_err());
}

#[test]
fn handler_panic_is_reported_after_every_other_handler_is_joined() {
    let gate = Arc::new(crate::lifecycle::Gate::default());
    let _release = crate::lifecycle::ReleaseOnDrop(gate.clone());
    let wait = gate.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let completed = Arc::new(AtomicBool::new(false));
    let observed = completed.clone();
    let server = FakeServer::start(move |request, _| {
        if request.raw.starts_with("GET /panic ") {
            panic!("one handler failed");
        }
        entered_tx.send(()).unwrap();
        assert!(wait.wait(Duration::from_secs(3)));
        observed.store(true, Ordering::SeqCst);
    });
    let mut slow = TcpStream::connect(server.addr()).unwrap();
    slow.write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut failed = TcpStream::connect(server.addr()).unwrap();
    failed
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    failed
        .write_all(b"GET /panic HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    assert_eq!(failed.read(&mut [0]).unwrap(), 0);
    let (done_tx, done_rx) = mpsc::channel();
    let dropper = std::thread::spawn(move || {
        let failed =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(server))).is_err();
        done_tx.send(failed).unwrap();
    });
    let early = done_rx.recv_timeout(Duration::from_millis(100)).ok();
    gate.release();
    let failed = early.unwrap_or_else(|| done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
    dropper.join().unwrap();
    assert!(
        early.is_none(),
        "panic must not detach an unfinished handler"
    );
    assert!(failed);
    assert!(completed.load(Ordering::SeqCst));
}

#[test]
fn handler_panic_during_unwind_child() {
    if std::env::var_os("FIXTURE_DOUBLE_PANIC_CHILD").is_none() {
        return;
    }
    let (entered_tx, entered_rx) = mpsc::channel();
    let result = std::panic::catch_unwind(|| {
        let server = FakeServer::start(move |_, _| {
            entered_tx.send(()).unwrap();
            panic!("handler failure during test unwind");
        });
        let mut stream = TcpStream::connect(server.addr()).unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let _server = server;
        panic!("original test failure");
    });
    assert_eq!(
        result.unwrap_err().downcast_ref::<&str>(),
        Some(&"original test failure")
    );
}

#[test]
fn handler_panic_during_test_unwind_preserves_the_original_failure() {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "lifecycle_connections::handler_panic_during_unwind_child",
        ])
        .env("FIXTURE_DOUBLE_PANIC_CHILD", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                status.success(),
                "fixture drop must preserve the original caught panic"
            );
            return;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("fixture unwind exceeded its shutdown deadline");
}
