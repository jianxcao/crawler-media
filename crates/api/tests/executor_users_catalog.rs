//! Admin User and catalog cache operations keep Store waits off Tokio.
#[path = "executor_users_catalog/fixture.rs"]
mod fixture;
#[path = "executor_users_catalog/views.rs"]
mod views;
use api::router;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use fixture::Fixture;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tower::ServiceExt;

fn request(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer users-cache-secret")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn route(f: &Fixture, index: usize) -> (&'static str, String, Value, u16) {
    match index {
        0 => ("GET", "/api/v1/users".into(), Value::Null, 200),
        1 => (
            "POST",
            "/api/v1/users".into(),
            json!({"login":"created","password":"created-password"}),
            201,
        ),
        2 => (
            "PATCH",
            format!("/api/v1/users/{}", f.member),
            json!({"login":"renamed"}),
            200,
        ),
        3 => (
            "PATCH",
            format!("/api/v1/users/{}", f.member),
            json!({"password":"changed-password"}),
            200,
        ),
        4 => (
            "DELETE",
            format!("/api/v1/users/{}", f.member),
            Value::Null,
            202,
        ),
        5 => (
            "GET",
            "/api/v1/users/removals/missing".into(),
            Value::Null,
            404,
        ),
        6 => ("GET", "/api/v1/catalog/cache".into(), Value::Null, 200),
        _ => (
            "DELETE",
            "/api/v1/catalog/cache?source=tmdb&cache_key=%2Fmovie%2F1".into(),
            Value::Null,
            200,
        ),
    }
}
fn hold_store(f: &Fixture, readonly: bool) -> std::thread::JoinHandle<()> {
    let store = f.state.store();
    let state = f.state.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let held = std::thread::spawn(move || {
        let _guard = store.lock();
        tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        if readonly {
            let _ = state.with_instance_writable(false);
        }
    });
    rx.recv_timeout(Duration::from_secs(3)).unwrap();
    held
}
#[test]
fn user_and_cache_routes_keep_executor_responsive() {
    let mut blocked = Vec::new();
    for index in 0..8 {
        let f = Fixture::new();
        let (method, uri, body, expected) = route(&f, index);
        let app = router(f.state.clone());
        let held = hold_store(&f, false);
        let (response, elapsed) = runtime().block_on(async {
            let started = Instant::now();
            tokio::join!(app.oneshot(request(method, &uri, body)), async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            })
        });
        held.join().unwrap();
        assert_eq!(
            response.unwrap().status().as_u16(),
            expected,
            "{method} {uri}"
        );
        if elapsed >= Duration::from_millis(200) {
            blocked.push(format!("{method} {uri}: {elapsed:?}"));
        }
    }
    assert!(blocked.is_empty(), "Store blocked executor: {blocked:?}");
}
#[test]
fn user_and_cache_mutations_recheck_admission_after_wait() {
    for index in [1, 2, 3, 4, 7] {
        let f = Fixture::new();
        let (method, uri, body, _) = route(&f, index);
        let app = router(f.state.clone());
        let held = hold_store(&f, true);
        let response = runtime()
            .block_on(app.oneshot(request(method, &uri, body)))
            .unwrap();
        held.join().unwrap();
        assert_eq!(response.status().as_u16(), 409, "{method} {uri}");
        let handle = f.state.store();
        let store = handle.lock();
        assert_eq!(store.get_user(f.member).unwrap().unwrap().login, "member");
        let credential = store.login_credential("member").unwrap().unwrap();
        assert!(store::password::verify_password(
            "original-password",
            &credential.observed_password
        ));
        assert!(store.user_by_token("member-session").unwrap().is_some());
        assert_eq!(store.list_users().unwrap().len(), 2);
        assert_eq!(store.list_catalog_cache().unwrap().len(), 1);
    }
}
#[test]
fn aggregate_cache_ignores_non_ascii_year_without_panicking() {
    let f = Fixture::new();
    let response = runtime()
        .block_on(router(f.state).oneshot(request(
            "GET",
            "/api/v1/catalog/cache?view=aggregate",
            Value::Null,
        )))
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let body: Value = runtime().block_on(async {
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
    });
    assert_eq!(body["data"][0]["title"], "Cached");
    assert!(body["data"][0]["year"].is_null());
}
