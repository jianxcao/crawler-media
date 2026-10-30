use super::app_writer::HeldAppWriter;
use super::doubles::PASSWORD;
use super::fixture::{Fixture, MEMBER_TOKEN};
use super::requests::{exchange, get, login};
use api::password_login::{PasswordEngine, PasswordLogin};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::ServiceExt;

struct Verified(tokio::sync::mpsc::UnboundedSender<()>);

impl PasswordEngine for Verified {
    fn verify(&self, password: &str, stored: &str) -> bool {
        let valid = password == PASSWORD && store::password::is_argon2_hash(stored);
        self.0.send(()).unwrap();
        valid
    }
}

fn password_request(jellyfin: bool) -> Request<Body> {
    if !jellyfin {
        return login();
    }
    Request::post("/Users/AuthenticateByName")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"Username": "interactive-member", "Pw": PASSWORD}).to_string(),
        ))
        .unwrap()
}

async fn assert_refused(response: axum::response::Response, maintenance: bool) {
    assert_eq!(
        response.status(),
        if maintenance {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::CONFLICT
        }
    );
    assert!(response.headers().get("set-cookie").is_none());
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    // Jellyfin admission refusals return an empty body; REST returns JSON.
    // Never include credential-bearing response bodies in assertion diagnostics.
    if !bytes.is_empty() {
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(body.get("AccessToken").is_none());
        assert!(
            body.get("data")
                .and_then(|data| data.get("token"))
                .is_none()
        );
    }
}

async fn queued_password_login(maintenance: bool, jellyfin: bool) {
    let fixture = Fixture::new();
    let (verified_tx, mut verified_rx) = tokio::sync::mpsc::unbounded_channel();
    let state = fixture
        .state
        .clone()
        .with_password_login(Arc::new(PasswordLogin::new(
            Arc::new(Verified(verified_tx)),
            1,
        )));
    let app = api::router(state.clone());
    let token_before = state
        .store()
        .lock()
        .latest_token_for_user(fixture.user)
        .unwrap();
    let held = HeldAppWriter::acquire(&state);
    let mut request = Box::pin(app.clone().oneshot(password_request(jellyfin)));
    // Poll both futures: verification must finish while the actual app writer is
    // held. No outer Store guard is involved in the obstruction.
    tokio::select! {
        response = &mut request => panic!("login finished before app writer release: {}", response.unwrap().status()),
        verified = verified_rx.recv() => assert!(verified.is_some()),
        _ = tokio::time::sleep(Duration::from_secs(2)) => panic!("credential read waited for app writer"),
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut request)
            .await
            .is_err()
    );
    let gate = state.write_gate();
    let closed = if maintenance {
        Some(gate.close())
    } else {
        let _ = state.clone().with_instance_writable(false);
        None
    };
    drop(held);
    assert_refused(request.await.unwrap(), maintenance).await;
    let token_after = state
        .store()
        .lock()
        .latest_token_for_user(fixture.user)
        .unwrap();
    assert!(
        token_after == token_before,
        "refused login must not persist a token"
    );
    drop(closed);
    let _ = state.clone().with_instance_writable(true);
    let successful = exchange(app.clone(), password_request(jellyfin), Instant::now()).await;
    assert_eq!(successful.status, StatusCode::OK);
    let token = if jellyfin {
        successful.body["AccessToken"].as_str().unwrap()
    } else {
        successful.body["data"]["token"].as_str().unwrap()
    };
    let authenticated = exchange(app, get("/api/v1/auth/me", token), Instant::now()).await;
    assert_eq!(authenticated.status, StatusCode::OK);
    assert_eq!(authenticated.body["data"]["id"], fixture.user.to_string());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_app_login_rechecks_admission_after_acquiring_its_writer() {
    for maintenance in [false, true] {
        for jellyfin in [false, true] {
            queued_password_login(maintenance, jellyfin).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_app_history_write_rechecks_admission_after_acquiring_its_writer() {
    for maintenance in [false, true] {
        let fixture = Fixture::new();
        let history_id = "app-admission-history";
        let history_key = format!("user.{}.search.history", fixture.user);
        let history = json!([{"id": history_id, "query": "preserve me"}]).to_string();
        {
            let handle = fixture.state.store();
            let store = handle.lock();
            store.put_setting(&history_key, &history).unwrap();
            store
                .insert_search_snapshot(history_id, "torrents", "preserve me", "{}", 1)
                .unwrap();
        }
        let app = api::router(fixture.state.clone());
        let held = HeldAppWriter::acquire(&fixture.state);
        let input = || {
            Request::delete(format!("/api/v1/search/history/{history_id}"))
                .header("authorization", format!("Bearer {MEMBER_TOKEN}"))
                .body(Body::empty())
                .unwrap()
        };
        let mut request = Box::pin(app.clone().oneshot(input()));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut request)
                .await
                .is_err()
        );
        let gate = fixture.state.write_gate();
        let closed = if maintenance {
            Some(gate.close())
        } else {
            let _ = fixture.state.clone().with_instance_writable(false);
            None
        };
        drop(held);
        assert_refused(request.await.unwrap(), maintenance).await;
        {
            let handle = fixture.state.store();
            let store = handle.lock();
            assert_eq!(
                store.get_setting(&history_key).unwrap().as_deref(),
                Some(history.as_str())
            );
            assert!(store.get_search_snapshot(history_id).unwrap().is_some());
        }
        drop(closed);
        let _ = fixture.state.clone().with_instance_writable(true);
        assert_eq!(app.oneshot(input()).await.unwrap().status(), StatusCode::OK);
        assert!(
            fixture
                .state
                .store()
                .lock()
                .get_search_snapshot(history_id)
                .unwrap()
                .is_none()
        );
    }
}

// A logout queued at the App writer must not revoke the session after the
// foreground gate closes. The session must still work until an admitted logout.
async fn queued_logout(maintenance: bool) {
    let fixture = Fixture::new();
    let app = api::router(fixture.state.clone());
    let input = || {
        Request::post("/api/v1/auth/logout")
            .header("authorization", format!("Bearer {MEMBER_TOKEN}"))
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        app.clone()
            .oneshot(get("/api/v1/auth/me", MEMBER_TOKEN))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let held = HeldAppWriter::acquire(&fixture.state);
    let mut request = Box::pin(app.clone().oneshot(input()));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut request)
            .await
            .is_err()
    );
    let gate = fixture.state.write_gate();
    let closed = if maintenance {
        Some(gate.close())
    } else {
        let _ = fixture.state.clone().with_instance_writable(false);
        None
    };
    drop(held);
    assert_refused(request.await.unwrap(), maintenance).await;
    assert_eq!(
        app.clone()
            .oneshot(get("/api/v1/auth/me", MEMBER_TOKEN))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    drop(closed);
    let _ = fixture.state.clone().with_instance_writable(true);
    assert_eq!(
        app.clone().oneshot(input()).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        app.oneshot(get("/api/v1/auth/me", MEMBER_TOKEN))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_app_logout_rechecks_maintenance_after_acquiring_its_writer() {
    queued_logout(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_app_logout_rechecks_instance_after_acquiring_its_writer() {
    queued_logout(false).await;
}

async fn queued_transfer_settings(maintenance: bool) {
    let fixture = Fixture::new();
    let app = api::router(fixture.state.clone());
    let before = fixture.state.store().lock().transfer_settings().unwrap();
    let input = || {
        Request::patch("/api/v1/settings/transfer")
            .header("authorization", "Bearer interactive-admin-fixture-token")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "expected_revision": before.revision,
                    "transfer_mode": "copy",
                    "scrape": true,
                })
                .to_string(),
            ))
            .unwrap()
    };
    let held = HeldAppWriter::acquire(&fixture.state);
    let mut request = Box::pin(app.clone().oneshot(input()));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut request)
            .await
            .is_err()
    );
    let gate = fixture.state.write_gate();
    let closed = if maintenance {
        Some(gate.close())
    } else {
        let _ = fixture.state.clone().with_instance_writable(false);
        None
    };
    drop(held);
    assert_refused(request.await.unwrap(), maintenance).await;
    assert_eq!(
        fixture.state.store().lock().transfer_settings().unwrap(),
        before
    );
    drop(closed);
    let _ = fixture.state.clone().with_instance_writable(true);
    assert_eq!(app.oneshot(input()).await.unwrap().status(), StatusCode::OK);
    let after = fixture.state.store().lock().transfer_settings().unwrap();
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.transfer_mode, "copy");
    assert!(after.scrape);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_app_transfer_settings_rechecks_maintenance_after_acquiring_its_writer() {
    queued_transfer_settings(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_app_transfer_settings_rechecks_instance_after_acquiring_its_writer() {
    queued_transfer_settings(false).await;
}
