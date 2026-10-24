use std::io::Write;
use std::net::TcpStream;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use crate::lifecycle::{Gate, ReleaseOnDrop};

fn dropping_owner_waits_for_handler<T: Send + Sync + 'static>(
    start: impl FnOnce(Box<dyn Fn() + Send + Sync>) -> Arc<T>,
    address: impl FnOnce(&T) -> String,
) {
    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(gate.clone());
    let wait = gate.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let mock = start(Box::new(move || {
        entered_tx.send(()).unwrap();
        assert!(
            wait.wait(Duration::from_secs(3)),
            "test must release the in-flight handler"
        );
    }));
    let address = address(&mock);
    let mut stream = TcpStream::connect(&address).unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let dropper = std::thread::spawn(move || {
        drop(mock);
        done_tx.send(()).unwrap();
    });
    let returned_before_handler = done_rx.recv_timeout(Duration::from_millis(100)).is_ok();
    gate.release();
    if !returned_before_handler {
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    dropper.join().unwrap();
    assert!(
        !returned_before_handler,
        "dropping the owner must join its in-flight handler"
    );
    assert!(
        TcpStream::connect(&address).is_err(),
        "the listener must be closed"
    );
}

#[test]
fn dropping_last_qbit_owner_joins_in_flight_handler() {
    let temp = tempfile::tempdir().unwrap();
    dropping_owner_waits_for_handler(
        |hook| crate::mock_qbit::MockQbit::start_with_handler_hook(temp.path(), hook),
        |mock| mock.url.trim_start_matches("http://").to_string(),
    );
}

#[test]
fn dropping_last_transmission_owner_joins_in_flight_handler() {
    let temp = tempfile::tempdir().unwrap();
    dropping_owner_waits_for_handler(
        |hook| {
            crate::mock_transmission::MockTransmission::start_with_handler_hook(
                temp.path(),
                16,
                hook,
            )
        },
        |mock| {
            mock.url
                .trim_start_matches("http://")
                .trim_end_matches("/transmission/rpc")
                .to_string()
        },
    );
}
