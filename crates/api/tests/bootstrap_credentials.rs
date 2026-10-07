use std::str::FromStr;
use std::sync::Arc;

use api::bootstrap_credentials::{
    AdminBootstrapAction, BootstrapCredentialsError, BootstrapMode, prepare_admin_credentials,
};
use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{User, UserId, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use parking_lot::Mutex;
use rusqlite::Connection;
use serde_json::{Value, json};
use tower::ServiceExt;

#[test]
fn production_fresh_install_rejects_missing_admin_password_before_any_user_row() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let err = prepare_admin_credentials(&store, "cli-token", None, BootstrapMode::Production)
        .unwrap_err();
    assert!(matches!(
        err,
        BootstrapCredentialsError::MissingAdminPassword
    ));
    assert!(
        store
            .get_user(UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn production_fresh_install_rejects_equal_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let err = prepare_admin_credentials(&store, "same", Some("same"), BootstrapMode::Production)
        .unwrap_err();
    assert!(matches!(
        err,
        BootstrapCredentialsError::PasswordEqualsToken
    ));
}

#[test]
fn test_fixture_mode_may_reuse_token_as_password() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let action = prepare_admin_credentials(
        &store,
        "management-secret",
        None,
        BootstrapMode::TestFixture,
    )
    .unwrap();
    match action {
        AdminBootstrapAction::Seed {
            password,
            cli_token,
        } => {
            assert_eq!(password, "management-secret");
            assert_eq!(cli_token, "management-secret");
        }
        _ => panic!("fixture must seed"),
    }
}

#[test]
fn production_insecure_legacy_requires_distinct_password() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let admin_id = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    store
        .insert_user(&User {
            id: admin_id,
            login: "admin".into(),
            enabled: true,
            role: UserRole::Admin,
        })
        .unwrap();
    store.set_password(admin_id, "legacy-token").unwrap();

    let err = prepare_admin_credentials(&store, "legacy-token", None, BootstrapMode::Production)
        .unwrap_err();
    assert!(matches!(
        err,
        BootstrapCredentialsError::InsecureLegacyNeedsPassword
    ));

    let ok_action = prepare_admin_credentials(
        &store,
        "legacy-token",
        Some("new-distinct-pw"),
        BootstrapMode::Production,
    )
    .unwrap();
    assert!(matches!(ok_action, AdminBootstrapAction::Rotate { .. }));
}

#[test]
fn production_existing_admin_with_empty_password_requires_password_setup() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let admin_id = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    // 管理员存在但密码为空串（未初始化安全密码）
    store
        .insert_user(&User {
            id: admin_id,
            login: "admin".into(),
            enabled: true,
            role: UserRole::Admin,
        })
        .unwrap();

    let err = prepare_admin_credentials(&store, "my-cli-token", None, BootstrapMode::Production)
        .unwrap_err();
    assert!(
        matches!(err, BootstrapCredentialsError::MissingAdminPassword),
        "管理员密码为空时绝不可放行 Keep，必须强制要求初始密码设置"
    );

    let action = prepare_admin_credentials(
        &store,
        "my-cli-token",
        Some("valid-admin-pw"),
        BootstrapMode::Production,
    )
    .unwrap();
    assert!(matches!(
        action,
        AdminBootstrapAction::Seed { .. } | AdminBootstrapAction::Rotate { .. }
    ));
}

#[test]
fn simultaneous_cli_and_admin_password_change() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let admin_id = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    store
        .insert_user(&User {
            id: admin_id,
            login: "admin".into(),
            enabled: true,
            role: UserRole::Admin,
        })
        .unwrap();
    // 旧实例：密码是旧 token A，并且 user_tokens 表中存有旧 token A
    store.set_password(admin_id, "old-cli-token-a").unwrap();
    store.set_user_token(admin_id, "old-cli-token-a").unwrap();

    // 场景 1：运维同时更换了新 token B，但未提供独立密码：必须拦截报错！
    let err = prepare_admin_credentials(&store, "new-cli-token-b", None, BootstrapMode::Production)
        .unwrap_err();
    assert!(
        matches!(err, BootstrapCredentialsError::InsecureLegacyNeedsPassword),
        "即使同时换了新 CLI Token，仍须检测出旧密码为不安全 token 并要求独立密码"
    );

    // 场景 2：运维同时更换了新 token B，并提供了独立新密码 C：必须触发 Rotate！
    let action = prepare_admin_credentials(
        &store,
        "new-cli-token-b",
        Some("new-independent-pass-c"),
        BootstrapMode::Production,
    )
    .unwrap();
    match action {
        AdminBootstrapAction::Rotate {
            password,
            cli_token,
        } => {
            assert_eq!(password, "new-independent-pass-c");
            assert_eq!(cli_token, "new-cli-token-b");
        }
        _ => panic!("必须触发 Rotate 升级管理员密码"),
    }
}

#[test]
fn replacement_password_cannot_reuse_old_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let admin_id = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    store
        .insert_user(&User {
            id: admin_id,
            login: "admin".into(),
            enabled: true,
            role: UserRole::Admin,
        })
        .unwrap();
    store.set_password(admin_id, "old-cli-token-a").unwrap();
    store.set_user_token(admin_id, "old-cli-token-a").unwrap();

    // 运维将 CLI 换成 B，却把 ADMIN_PASSWORD 设为了旧的 CLI 密钥 A
    let res = prepare_admin_credentials(
        &store,
        "new-cli-token-b",
        Some("old-cli-token-a"),
        BootstrapMode::Production,
    );
    assert!(
        matches!(
            res,
            Err(BootstrapCredentialsError::PasswordEqualsKnownBearer)
        ),
        "绝不可将旧系统 CLI Bearer 密钥重新设为管理员登录密码"
    );
}

#[test]
fn legacy_detection_survives_admin_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let admin_id = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    // 管理员账号曾被重命名为 "root-admin"
    store
        .insert_user(&User {
            id: admin_id,
            login: "root-admin".into(),
            enabled: true,
            role: UserRole::Admin,
        })
        .unwrap();
    store.set_password(admin_id, "legacy-token-a").unwrap();
    store.set_user_token(admin_id, "legacy-token-a").unwrap();

    // 生产启动：换了新 token B，但未提供独立密码
    let res = prepare_admin_credentials(&store, "new-token-b", None, BootstrapMode::Production);
    assert!(
        matches!(
            res,
            Err(BootstrapCredentialsError::InsecureLegacyNeedsPassword)
        ),
        "重命名管理员后，必须依然能够识别其不安全的旧 token 密码并强制要求独立密码"
    );
}

#[tokio::test]
async fn seed_admin_rolls_back_when_token_insert_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Arc::new(Mutex::new(Store::open(&data_dir).unwrap()));

    // 触发器拦截对 user_tokens 的写入
    let conn = Connection::open(data_dir.join("app.db")).unwrap();
    conn.execute(
        "CREATE TRIGGER reject_seed_token BEFORE INSERT ON user_tokens BEGIN SELECT RAISE(FAIL, 'reject'); END;",
        [],
    ).unwrap();
    drop(conn);

    let res = ApiState::new_arc_with_admin_password(
        store.clone(),
        "cli-token".to_string(),
        Some("admin-password".to_string()),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoopFetcher),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    );
    assert!(res.is_err());

    // 验证用户未被部分写入
    let admin_id = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    assert!(
        store.lock().get_user(admin_id).unwrap().is_none(),
        "seed 失败绝不残留管理员行"
    );
}

fn build_test_app(store: Arc<Mutex<Store>>, tmp: &tempfile::TempDir, token: &str) -> axum::Router {
    let state = ApiState::new_arc_with_admin_password(
        store,
        token.to_string(),
        Some("independent-password".to_string()),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoopFetcher),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap();
    router(state)
}

#[tokio::test]
async fn cli_token_rotates_and_refreshes_on_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");

    // 首次启动：指定 token-a
    let store = Arc::new(Mutex::new(Store::open(&data_dir).unwrap()));
    let app = build_test_app(store.clone(), &tmp, "token-a");

    let me_a = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/v1/auth/me")
                .header("authorization", "Bearer token-a")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me_a.status(), StatusCode::OK);

    // 重启：密码未变（进入 Keep 分支），但运维在环境变量中更换了 Token 为 token-b
    let app2 = build_test_app(store.clone(), &tmp, "token-b");

    let me_b = app2
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/v1/auth/me")
                .header("authorization", "Bearer token-b")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        me_b.status(),
        StatusCode::OK,
        "重启后新的 CLI Token 必须立即可用"
    );

    let me_a_stale = app2
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/v1/auth/me")
                .header("authorization", "Bearer token-a")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        me_a_stale.status(),
        StatusCode::UNAUTHORIZED,
        "轮换后旧的 token-a 必须被立即撤销失效"
    );
}

struct NoopFetcher;
impl Fetcher for NoopFetcher {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Ok("{}".into())
    }
}

fn request(method: &str, uri: &str, body: Value) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn bootstrap_admin_password_and_cli_token_are_separated() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Arc::new(Mutex::new(Store::open(&data_dir).unwrap()));

    let cli_token = "cli-bearer-secret-999";
    let admin_pw = "admin-login-pwd-888";

    let state = ApiState::new_arc_with_admin_password(
        store.clone(),
        cli_token.to_string(),
        Some(admin_pw.to_string()),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoopFetcher),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap();
    let app = router(state);

    // 1. 初始化刚完成时，检查数据库 user_tokens 表中只有 cli_token，绝对没有明文 admin_pw！
    let conn = Connection::open(data_dir.join("app.db")).unwrap();
    let tokens: Vec<String> = conn
        .prepare("SELECT token FROM user_tokens")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(tokens, vec![cli_token.to_string()]);
    assert!(!tokens.contains(&admin_pw.to_string()));

    // 2. 用 admin_password 登录成功
    let login_pw = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            json!({ "username": "admin", "password": admin_pw }),
        ))
        .await
        .unwrap();
    assert_eq!(login_pw.status(), StatusCode::OK);

    // 3. 用 cli_token 作为密码登录必须失败！
    let login_token_as_pw = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            json!({ "username": "admin", "password": cli_token }),
        ))
        .await
        .unwrap();
    assert_eq!(login_token_as_pw.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn legacy_instance_with_token_as_password_upgrades_safely() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");

    // 模拟旧版本数据库：admin 的 password 等于 old-token
    let old_token = "old-shared-secret-111";
    let store_raw = Store::open(&data_dir).unwrap();
    let admin_id = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    let conn = Connection::open(data_dir.join("app.db")).unwrap();
    conn.execute(
        "INSERT INTO users (id, login, password, enabled, role) VALUES (?1, 'admin', ?2, 1, 'admin')",
        rusqlite::params![admin_id.to_string(), old_token],
    ).unwrap();
    conn.execute(
        "INSERT INTO user_tokens (token, user_id, created_at) VALUES (?1, ?2, 0)",
        rusqlite::params![old_token, admin_id.to_string()],
    )
    .unwrap();
    drop(conn);

    let store = Arc::new(Mutex::new(store_raw));
    let new_admin_pw = "new-distinct-admin-password";

    // 重新启动服务并提供独立的 new_admin_pw
    let state = ApiState::new_arc_with_admin_password(
        store.clone(),
        old_token.to_string(),
        Some(new_admin_pw.to_string()),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoopFetcher),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap();
    let app = router(state);

    // 验证新密码能正常登录
    let login_new = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            json!({ "username": "admin", "password": new_admin_pw }),
        ))
        .await
        .unwrap();
    assert_eq!(login_new.status(), StatusCode::OK);

    // 验证旧 token 不能再被当作密码登录
    let login_old = app
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            json!({ "username": "admin", "password": old_token }),
        ))
        .await
        .unwrap();
    assert_eq!(login_old.status(), StatusCode::UNAUTHORIZED);
}
