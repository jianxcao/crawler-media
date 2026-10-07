use std::collections::HashMap;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::{Fixtures, json_body, request, state as common_state};

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn authed_app(tmp: &tempfile::TempDir) -> axum::Router {
    router(common_state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

fn bearer(token: &str) -> axum::http::Request<axum::body::Body> {
    request("GET", "/api/v1/auth/me", Some(token), Value::Null)
}

async fn login_token(app: &axum::Router, username: &str, password: &str) -> String {
    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": username, "password": password }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    json_body(res).await["data"]["token"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn create_member(app: &axum::Router, login: &str, password: &str) -> (String, String) {
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": login, "password": password }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = json_body(created).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let token = login_token(app, login, password).await;
    (id, token)
}

#[tokio::test]
async fn logout_revokes_only_the_presented_session() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let first = login_token(&app, "admin", "management-secret").await;
    let second = login_token(&app, "admin", "management-secret").await;

    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/logout",
            Some(&first),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let revoked = app.clone().oneshot(bearer(&first)).await.unwrap();
    assert_eq!(
        revoked.status(),
        StatusCode::UNAUTHORIZED,
        "logout must revoke the token"
    );

    // 多会话：另一个设备不受影响。
    let survivor = app.clone().oneshot(bearer(&second)).await.unwrap();
    assert_eq!(survivor.status(), StatusCode::OK);
}

#[tokio::test]
async fn password_change_revokes_existing_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let (alice_id, alice_token) = create_member(&app, "alice", "alice-pass").await;
    let admin = login_token(&app, "admin", "management-secret").await;

    let patched = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/users/{alice_id}"),
            Some(&admin),
            json!({ "password": "new-alice-password" }),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);

    let old_alice = app.clone().oneshot(bearer(&alice_token)).await.unwrap();
    assert_eq!(
        old_alice.status(),
        StatusCode::UNAUTHORIZED,
        "password change must revoke the member's existing tokens"
    );

    let relogin = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": "alice", "password": "new-alice-password" }),
        ))
        .await
        .unwrap();
    assert_eq!(relogin.status(), StatusCode::OK);

    let failed_login = app
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": "alice", "password": "alice-pass" }),
        ))
        .await
        .unwrap();
    assert_eq!(failed_login.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn expired_session_tokens_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user_id = domain::UserId::new();
    let user = domain::User {
        id: user_id,
        login: "exp-user".into(),
        enabled: true,
        role: domain::UserRole::Member,
    };
    store.insert_user(&user).unwrap();

    let conn = rusqlite::Connection::open(tmp.path().join("data").join("app.db")).unwrap();
    let old_ts = 1_000_000_000i64;
    conn.execute(
        "INSERT INTO user_tokens (token, user_id, created_at) VALUES ('expired-token', ?1, ?2)",
        rusqlite::params![user_id.to_string(), old_ts],
    )
    .unwrap();

    let resolved = store.user_by_token("expired-token").unwrap();
    assert!(resolved.is_none(), "expired session token must not resolve");
}

#[tokio::test]
async fn logout_with_invalid_bearer_and_valid_cookie_revokes_cookie_session() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let (_alice_id, alice_token) = create_member(&app, "alice", "alice-pass").await;

    // 客户端发送 logout：同时携带了失效的 Bearer 头与有效的 Cookie
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/v1/auth/logout")
        .header("authorization", "Bearer stale-invalid-token")
        .header("cookie", format!("mc_session={alice_token}"))
        .body(axum::body::Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 验证数据库中该有效的 alice_token 确实被撤销失效了，不可再访问接口
    let check = app.oneshot(bearer(&alice_token)).await.unwrap();
    assert_eq!(
        check.status(),
        StatusCode::UNAUTHORIZED,
        "即使请求带了无效的 Bearer 头，有效的 Cookie 会话也必须被安全撤销"
    );
}
