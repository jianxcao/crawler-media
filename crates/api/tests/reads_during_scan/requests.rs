use api::ApiState;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use tower::ServiceExt;

use super::doubles::PASSWORD;

pub fn get(uri: &str, token: &str) -> Request<Body> {
    Request::get(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

pub fn login() -> Request<Body> {
    Request::post("/api/v1/auth/login")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"username": "interactive-member", "password": PASSWORD}).to_string(),
        ))
        .unwrap()
}

pub struct Response {
    pub status: StatusCode,
    pub elapsed: Duration,
    pub body: Value,
}

pub async fn exchange(app: axum::Router, request: Request<Body>, started: Instant) -> Response {
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    Response {
        status,
        elapsed: started.elapsed(),
        body: serde_json::from_slice(&bytes).unwrap(),
    }
}

/// The real outer writer is held for three seconds, even on an assertion path.
/// No early-release channel can accidentally let a request satisfy the test.
pub struct HeldWriter {
    held: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl HeldWriter {
    pub fn acquire(state: &ApiState) -> Self {
        let writer = state.store();
        let held = Arc::new(AtomicBool::new(false));
        let marker = held.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let guard = crate::writer_hold::WriterHold::acquire(writer.clone(), store::Db::Library);
            marker.store(true, Ordering::SeqCst);
            ready_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_secs(3));
            marker.store(false, Ordering::SeqCst);
            drop(guard);
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        Self {
            held,
            worker: Some(worker),
        }
    }

    pub fn is_held(&self) -> bool {
        self.held.load(Ordering::SeqCst)
    }

    pub fn finish(mut self) {
        self.worker.take().unwrap().join().unwrap();
    }
}

impl Drop for HeldWriter {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
