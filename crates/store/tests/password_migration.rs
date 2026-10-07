use domain::{User, UserId, UserRole};
use rusqlite::{Connection, params};
use store::Store;

#[test]
fn literal_hash_prefix_is_always_hashed() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user_id = UserId::new();
    let user = User {
        id: user_id,
        login: "prefix_user".into(),
        enabled: true,
        role: UserRole::Member,
    };
    store.insert_user(&user).unwrap();

    let tricky_pw = "$argon2id$not-a-valid-phc";
    store.set_password(user_id, tricky_pw).unwrap();

    // 从数据库直查 users.password
    let conn = Connection::open(tmp.path().join("app.db")).unwrap();
    let stored_pw: String = conn
        .query_row(
            "SELECT password FROM users WHERE id = ?1",
            params![user_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();

    // 绝不能直接把未经哈希的明文 tricky_pw 存进去
    assert_ne!(
        stored_pw, tricky_pw,
        "raw input must never be stored verbatim"
    );
    assert!(
        stored_pw.starts_with("$argon2id$"),
        "stored password must be a valid argon2id hash: {stored_pw}"
    );

    // 用用户输入的原始密码 tricky_pw 登录必须成功
    let login_res = store
        .verify_user_password("prefix_user", tricky_pw)
        .unwrap();
    assert!(
        login_res.is_some(),
        "login with literal hash prefix password must succeed"
    );
}

#[test]
fn passwords_generate_different_salts() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user1 = UserId::new();
    let user2 = UserId::new();
    store
        .insert_user(&User {
            id: user1,
            login: "u1".into(),
            enabled: true,
            role: UserRole::Member,
        })
        .unwrap();
    store
        .insert_user(&User {
            id: user2,
            login: "u2".into(),
            enabled: true,
            role: UserRole::Member,
        })
        .unwrap();

    let pw = "$argon2id$same-secret";
    store.set_password(user1, pw).unwrap();
    store.set_password(user2, pw).unwrap();

    let conn = Connection::open(tmp.path().join("app.db")).unwrap();
    let p1: String = conn
        .query_row(
            "SELECT password FROM users WHERE id = ?1",
            params![user1.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let p2: String = conn
        .query_row(
            "SELECT password FROM users WHERE id = ?1",
            params![user2.to_string()],
            |r| r.get(0),
        )
        .unwrap();

    assert_ne!(p1, p2, "each password hash must have a unique random salt");
}

#[test]
fn upgrade_failure_fails_closed_and_rolls_back_token() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user_id = UserId::new();
    let conn = Connection::open(tmp.path().join("app.db")).unwrap();

    // 插入旧明文用户
    conn.execute(
        "INSERT INTO users (id, login, password, enabled, role) VALUES (?1, 'failing_user', 'plain-secret-1', 1, 'member')",
        params![user_id.to_string()],
    )
    .unwrap();

    // 安装触发器注入更新失败
    conn.execute(
        "CREATE TRIGGER reject_password_upgrade BEFORE UPDATE OF password ON users
         BEGIN
             SELECT RAISE(FAIL, 'reject upgrade');
         END;",
        [],
    )
    .unwrap();

    // 登录必须返回 Err（fail closed），绝不能静默成功并放行明文
    let verify_result = store.verify_user_password("failing_user", "plain-secret-1");
    assert!(verify_result.is_err(), "upgrade failure must return Err");

    // 验证数据库中未签发 token
    let token_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM user_tokens WHERE user_id = ?1",
            params![user_id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        token_count, 0,
        "no token should be issued when upgrade fails"
    );

    // 移除触发器后，登录成功且升级
    conn.execute("DROP TRIGGER reject_password_upgrade", [])
        .unwrap();
    let success_login = store
        .verify_user_password("failing_user", "plain-secret-1")
        .unwrap();
    assert!(success_login.is_some());
    let upgraded_pw: String = conn
        .query_row(
            "SELECT password FROM users WHERE id = ?1",
            params![user_id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(upgraded_pw.starts_with("$argon2id$"));
}

#[test]
fn concurrent_password_change_causes_cas_rejection_without_token() {
    let tmp = tempfile::tempdir().unwrap();
    let store1 = Store::open(tmp.path()).unwrap();
    let store2 = Store::open(tmp.path()).unwrap();
    let user_id = UserId::new();
    let conn = Connection::open(tmp.path().join("app.db")).unwrap();

    // 插入旧明文用户
    conn.execute(
        "INSERT INTO users (id, login, password, enabled, role) VALUES (?1, 'cas_user', 'old-password', 1, 'member')",
        params![user_id.to_string()],
    )
    .unwrap();

    // 模拟连接 A 读到旧密码后，连接 B 并发修改了密码
    store2.set_password(user_id, "new-password-from-b").unwrap();

    // 此时尝试用连接 A 传入已失效的 observed_password 'old-password' 执行登录升级
    // 应该因为 CAS 匹配不满足返回 None，并且不签发 token
    let finish_res = store1
        .finish_verified_login(
            &user_id.to_string(),
            "old-password",
            Some("$argon2id$some-new-hash"),
        )
        .unwrap();
    assert!(finish_res.is_none(), "stale login must be rejected by CAS");

    // 验证新密码仍可正常登录
    let login_b = store1
        .verify_user_password("cas_user", "new-password-from-b")
        .unwrap();
    assert!(login_b.is_some(), "new password must remain valid");
}

#[test]
fn rotate_admin_password_rolls_back_when_token_insert_fails() {
    use std::str::FromStr;
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let id = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    store
        .insert_user(&User {
            id,
            login: "admin".into(),
            enabled: true,
            role: UserRole::Admin,
        })
        .unwrap();
    store.set_password(id, "old-token").unwrap();
    store.set_user_token(id, "old-token").unwrap();
    let conn = Connection::open(tmp.path().join("app.db")).unwrap();
    conn.execute(
        "CREATE TRIGGER reject_token BEFORE INSERT ON user_tokens BEGIN SELECT RAISE(FAIL, 'no token'); END;",
        [],
    ).unwrap();
    let err = store.rotate_admin_password_keeping_cli_token(id, "new-admin-pass", "old-token");
    assert!(err.is_err());
    assert!(
        store
            .password_matches_without_session("admin", "old-token")
            .unwrap()
    );
    assert!(
        !store
            .password_matches_without_session("admin", "new-admin-pass")
            .unwrap()
    );
}

#[test]
fn rotate_admin_password_revokes_old_cli_bearer() {
    use std::str::FromStr;
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let id = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    store
        .insert_user(&User {
            id,
            login: "admin".into(),
            enabled: true,
            role: UserRole::Admin,
        })
        .unwrap();
    store
        .register_current_cli_token(id, "old-bearer-a")
        .unwrap();
    store.set_user_token(id, "ordinary-session-xyz").unwrap();

    // 旋转密码并同时换新 CLI Token 为 new-bearer-b
    store
        .rotate_admin_password_keeping_cli_token(id, "new-secure-pass", "new-bearer-b")
        .unwrap();

    // 断言：new-bearer-b 生效，old-bearer-a 和普通会话必须被立即撤销！
    assert!(store.user_by_token("new-bearer-b").unwrap().is_some());
    assert!(
        store.user_by_token("old-bearer-a").unwrap().is_none(),
        "Rotate 时必须原子撤销旧 CLI Bearer"
    );
    assert!(
        store
            .user_by_token("ordinary-session-xyz")
            .unwrap()
            .is_none(),
        "密码轮换时必须撤销管理员名下普通历史会话"
    );
}

#[test]
fn cli_token_collision_with_member_session_fails_closed() {
    use std::str::FromStr;
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let admin_id = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    let member_id = UserId::new();

    store
        .insert_user(&User {
            id: admin_id,
            login: "admin".into(),
            enabled: true,
            role: UserRole::Admin,
        })
        .unwrap();
    store
        .insert_user(&User {
            id: member_id,
            login: "member".into(),
            enabled: true,
            role: UserRole::Member,
        })
        .unwrap();

    // 成员先行持有了 token "colliding-token"
    store.set_user_token(member_id, "colliding-token").unwrap();

    // 试图将管理员 CLI Token 登记为该已存在的成员 token：必须被拦截并回滚！
    let res = store.register_current_cli_token(admin_id, "colliding-token");
    assert!(res.is_err(), "与已有成员 Token 碰撞时必须返回错误并回滚");

    // 验证归属没有被静默篡改为管理员
    let user = store.user_by_token("colliding-token").unwrap().unwrap();
    assert_eq!(
        user.id, member_id,
        "碰撞发生后该 token 必须依然属于成员，不能被提权"
    );
    assert_eq!(user.role, UserRole::Member);
}

#[test]
fn seed_admin_recovery_revokes_legacy_ordinary_sessions() {
    use std::str::FromStr;
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let admin_id = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();

    // 预先建立旧 marker
    store
        .register_current_cli_token(admin_id, "cli-token")
        .unwrap();

    // 模拟管理员密码被清空损坏，但残留了一个普通会话
    store
        .insert_user(&User {
            id: admin_id,
            login: "admin".into(),
            enabled: true,
            role: UserRole::Admin,
        })
        .unwrap();
    store.set_user_token(admin_id, "stale-session-123").unwrap();
    assert!(store.user_by_token("stale-session-123").unwrap().is_some());

    // 重新 Seed 恢复初始密码
    store
        .seed_admin_with_credentials(admin_id, "brand-new-pass", "cli-token")
        .unwrap();

    // 验证：cli-token 可用，但旧残留会话必须被彻底撤销！
    assert!(store.user_by_token("cli-token").unwrap().is_some());
    assert!(
        store.user_by_token("stale-session-123").unwrap().is_none(),
        "密码重设恢复必须撤销旧残留会话"
    );
}

#[test]
fn delete_session_tokens_is_atomic_and_protects_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user_id = domain::UserId::new();
    store
        .insert_user(&User {
            id: user_id,
            login: "bob".into(),
            enabled: true,
            role: UserRole::Member,
        })
        .unwrap();

    store.set_user_token(user_id, "token-1").unwrap();
    store.set_user_token(user_id, "token-2").unwrap();

    let conn = Connection::open(tmp.path().join("app.db")).unwrap();
    conn.execute(
        "CREATE TRIGGER reject_token_2 BEFORE DELETE ON user_tokens
         WHEN OLD.token = 'token-2'
         BEGIN
             SELECT RAISE(FAIL, 'cannot delete token-2');
         END;",
        [],
    )
    .unwrap();

    // 尝试批量删除 token-1 和 token-2，token-2 触发错误时 token-1 绝不能被部分删除
    let res = store.delete_session_tokens(&["token-1".to_string(), "token-2".to_string()]);
    assert!(res.is_err(), "遇到错误必须失败回滚");

    // 验证事务原子性：token-1 依然存在！
    assert!(
        store.user_by_token("token-1").unwrap().is_some(),
        "回滚后 token-1 必须保留，不能部分生效"
    );
    assert!(store.user_by_token("token-2").unwrap().is_some());
}
