use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::{
    Coverage, FetchMode, FilterId, MediaId, Subscribe, SubscribeId, User, UserId, UserRole,
};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

async fn create_member(app: &axum::Router) -> (String, String) {
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": "alice", "password": "alice-pass" }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = json_body(created).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let login = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": "alice", "password": "alice-pass" }),
        ))
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let token = json_body(login).await["data"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    (id, token)
}

#[tokio::test]
async fn disabling_member_revokes_sessions_and_blocks_login() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (id, token) = create_member(&app).await;

    let disabled = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/users/{id}"),
            Some("management-secret"),
            json!({ "enabled": false }),
        ))
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::OK);
    assert_eq!(json_body(disabled).await["data"]["enabled"], false);

    let old_session = app
        .clone()
        .oneshot(request("GET", "/api/v1/auth/me", Some(&token), Value::Null))
        .await
        .unwrap();
    assert_eq!(old_session.status(), StatusCode::UNAUTHORIZED);
    let relogin = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": "alice", "password": "alice-pass" }),
        ))
        .await
        .unwrap();
    assert_eq!(relogin.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_users_role_comes_from_db_not_seed_uuid() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": "second-admin", "password": "pw" }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = json_body(created).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let data = tmp.path().join("data");
    Connection::open(data.join("app.db"))
        .unwrap()
        .execute("UPDATE users SET role = 'admin' WHERE id = ?1", params![id])
        .unwrap();

    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/users",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let users = json_body(listed).await["data"].as_array().unwrap().clone();
    let row = users.iter().find(|u| u["id"] == id).expect("created user");
    assert_eq!(
        row["role"], "admin",
        "role must follow users.role, not seed UUID"
    );
}

#[tokio::test]
async fn admin_cannot_be_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let response = app
        .oneshot(request(
            "PATCH",
            "/api/v1/users/00000000-0000-0000-0000-000000000001",
            Some("management-secret"),
            json!({ "enabled": false }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn deleting_member_transfers_owned_records_and_clears_private_state() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let store = Store::open(&data).unwrap();
    let admin = User {
        id: UserId::from_str("10000000-0000-0000-0000-000000000000").unwrap(),
        login: "z-admin".into(),
        enabled: true,
        role: UserRole::Admin,
    };
    store.insert_user(&admin).unwrap();
    Connection::open(data.join("app.db"))
        .unwrap()
        .execute(
            "UPDATE users SET role = 'admin' WHERE id = ?1",
            params![admin.id.to_string()],
        )
        .unwrap();
    let member = User {
        id: UserId::new(),
        login: "alice".into(),
        enabled: true,
        role: UserRole::Member,
    };
    store.insert_user(&member).unwrap();
    store.set_password(member.id, "pw").unwrap();
    store.set_user_token(member.id, "session").unwrap();
    let subscribe = Subscribe {
        id: SubscribeId::new(),
        user_id: member.id,
        media_id: MediaId::new(),
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    store.insert_subscribe(&subscribe).unwrap();
    let collection = store.create_collection(member.id, "片单").unwrap();
    store
        .add_collection_item(&collection, &subscribe.media_id.to_string())
        .unwrap();
    store
        .set_playback_progress(member.id, subscribe.media_id, 123)
        .unwrap();

    assert!(store.delete_user(member.id).unwrap());
    assert_eq!(
        store.get_subscribe(subscribe.id).unwrap().unwrap().user_id,
        admin.id
    );
    assert_eq!(store.list_collections(admin.id).unwrap().len(), 1);
    assert_eq!(store.collection_item_ids(&collection).unwrap().len(), 1);
    assert!(store.get_user(member.id).unwrap().is_none());
    assert!(store.user_by_token("session").unwrap().is_none());
    assert!(
        store
            .playback_progress(member.id, subscribe.media_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn delete_subscribe_clears_all_related_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let store = Store::open(&data).unwrap();
    let id = SubscribeId::new();
    let subscribe = Subscribe {
        id,
        user_id: UserId::new(),
        media_id: MediaId::new(),
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    store.insert_subscribe(&subscribe).unwrap();
    let conn = Connection::open(data.join("subscribe.db")).unwrap();
    conn.execute(
        "INSERT INTO subscribe_facts (subscribe_id, score) VALUES (?1, 1)",
        params![id.to_string()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO subscribe_wanted (subscribe_id) VALUES (?1)",
        params![id.to_string()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO pending_downloads (subscribe_id, enclosure, title, score, torrent_json) VALUES (?1, 'e', 't', 1, '{}')",
        params![id.to_string()],
    ).unwrap();

    assert!(store.delete_subscribe(id).unwrap());
    let subscribe_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM subscribes WHERE id = ?1",
            params![id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(subscribe_count, 0, "subscribes");
    for table in ["subscribe_facts", "subscribe_wanted", "pending_downloads"] {
        let count: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE subscribe_id = ?1"),
                params![id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}

#[tokio::test]
async fn user_password_is_stored_as_argon2_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": "hash-user", "password": "super-secret-pw" }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = json_body(created).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let conn = Connection::open(tmp.path().join("data").join("app.db")).unwrap();
    let stored_pw: String = conn
        .query_row(
            "SELECT password FROM users WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        stored_pw.starts_with("$argon2id$"),
        "password must be stored as Argon2id hash: {stored_pw}"
    );
    assert_ne!(stored_pw, "super-secret-pw");

    // Login with the correct password works
    let login_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": "hash-user", "password": "super-secret-pw" }),
        ))
        .await
        .unwrap();
    assert_eq!(login_res.status(), StatusCode::OK);

    // Login with wrong password fails
    let fail_res = app
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": "hash-user", "password": "wrong-password" }),
        ))
        .await
        .unwrap();
    assert_eq!(fail_res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn legacy_plaintext_password_is_transparently_upgraded_on_login() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let data_db = tmp.path().join("data").join("app.db");

    // Simulate an existing database with a legacy unhashed plaintext password
    let conn = Connection::open(&data_db).unwrap();
    let user_id = domain::UserId::new();
    conn.execute(
        "INSERT INTO users (id, login, password, enabled, role) VALUES (?1, 'legacy-user', 'plain-text-secret', 1, 'member')",
        params![user_id.to_string()],
    ).unwrap();

    // Verify it is plain text initially
    let initial_pw: String = conn
        .query_row(
            "SELECT password FROM users WHERE id = ?1",
            params![user_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(initial_pw, "plain-text-secret");

    // Login with the plaintext password succeeds
    let login_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": "legacy-user", "password": "plain-text-secret" }),
        ))
        .await
        .unwrap();
    assert_eq!(login_res.status(), StatusCode::OK);

    // Check that the password in database has been automatically upgraded to Argon2id!
    let upgraded_pw: String = conn
        .query_row(
            "SELECT password FROM users WHERE id = ?1",
            params![user_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        upgraded_pw.starts_with("$argon2id$"),
        "password must be upgraded to Argon2id hash: {upgraded_pw}"
    );

    // Next login with the same password still succeeds using the upgraded hash
    let login_again = app
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": "legacy-user", "password": "plain-text-secret" }),
        ))
        .await
        .unwrap();
    assert_eq!(login_again.status(), StatusCode::OK);
}

#[tokio::test]
async fn create_user_rolls_back_when_password_insert_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let conn = Connection::open(tmp.path().join("data").join("app.db")).unwrap();

    // 触发器：拦截用户名为 boom 的插入
    conn.execute(
        "CREATE TRIGGER reject_boom BEFORE INSERT ON users WHEN NEW.login = 'boom'
         BEGIN
             SELECT RAISE(FAIL, 'nope');
         END;",
        [],
    )
    .unwrap();

    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": "boom", "password": "some-password" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // 验证用户表中未残留 boom 行
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM users WHERE login = 'boom'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        count, 0,
        "failed create_user must not leave partial user row"
    );
}

#[tokio::test]
async fn patch_password_rolls_back_when_token_delete_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (alice_id, alice_token) = create_member(&app).await;
    let conn = Connection::open(tmp.path().join("data").join("app.db")).unwrap();

    // 触发器：拦截对 user_tokens 的删除操作，模拟级联清除失败
    conn.execute(
        "CREATE TRIGGER reject_token_del BEFORE DELETE ON user_tokens
         BEGIN
             SELECT RAISE(FAIL, 'keep');
         END;",
        [],
    )
    .unwrap();

    let patch_res = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/users/{alice_id}"),
            Some("management-secret"),
            json!({ "password": "new-alice-password" }),
        ))
        .await
        .unwrap();
    assert_eq!(patch_res.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // 验证事务已完整回滚：原 token 依然有效，新口令不可登录
    let session_check = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/auth/me",
            Some(&alice_token),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(session_check.status(), StatusCode::OK);

    let login_new = app
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": "alice", "password": "new-alice-password" }),
        ))
        .await
        .unwrap();
    assert_eq!(login_new.status(), StatusCode::UNAUTHORIZED);
}

async fn assert_cli_persists_after_logout(app: axum::Router) {
    let logout_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/logout",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(logout_res.status(), StatusCode::OK);

    let cli_after_logout = app
        .oneshot(request(
            "GET",
            "/api/v1/auth/me",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(
        cli_after_logout.status(),
        StatusCode::OK,
        "登出绝不能注销系统 CLI Token"
    );
}

#[tokio::test]
async fn admin_password_patch_keeps_cli_and_revokes_web_session() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let admin_id = "00000000-0000-0000-0000-000000000001";

    let login = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": "admin", "password": "management-secret" }),
        ))
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let web_session = json_body(login).await["data"]["token"]
        .as_str()
        .unwrap()
        .to_string();

    let patched = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/users/{admin_id}"),
            Some("management-secret"),
            json!({ "password": "brand-new-admin-password" }),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);

    let session_check = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/auth/me",
            Some(&web_session),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(session_check.status(), StatusCode::UNAUTHORIZED);

    let cli_check = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/auth/me",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(
        cli_check.status(),
        StatusCode::OK,
        "管理员改密绝不能撤销已配置的系统 CLI Token"
    );

    assert_cli_persists_after_logout(app).await;
}

#[tokio::test]
async fn admin_password_cannot_equal_current_cli_token() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let admin_id = "00000000-0000-0000-0000-000000000001";

    // 尝试将管理员密码修改为当前系统的 CLI token "management-secret"
    let response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/users/{admin_id}"),
            Some("management-secret"),
            json!({ "password": "management-secret" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "管理员口令不能被修改为当前系统 CLI Token"
    );

    let err = json_body(response).await;
    assert_eq!(err["error"]["code"], "user.credential_conflict");

    // 验证原密码依然有效，未被破坏
    let check = app
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": "admin", "password": "management-secret" }),
        ))
        .await
        .unwrap();
    assert_eq!(check.status(), StatusCode::OK);
}
