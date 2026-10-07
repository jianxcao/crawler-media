use super::*;

#[tokio::test]
async fn login_returns_token_and_user() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        None,
        json!({ "username": "admin", "password": "management-secret" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap();
    assert!(cookie.starts_with("mc_session="));
    assert!(cookie.contains("HttpOnly"));
    let body = json_body(response).await;
    assert_eq!(body["ok"], true);
    // 登录签发独立会话 token（不再是密码本身）。
    assert!(
        body["data"]["token"]
            .as_str()
            .is_some_and(|token| !token.is_empty()),
        "login must issue a session token"
    );
    assert_eq!(body["data"]["user"]["login"], "admin");
}

#[tokio::test]
async fn login_rejects_wrong_password() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        None,
        json!({ "username": "admin", "password": "nope" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(response).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"]["code"], "auth.invalid");
}

#[tokio::test]
async fn protected_requires_bearer() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(&app, "GET", "/api/v1/users", None, Value::Null).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(response).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"]["code"], "auth.required");
}

#[tokio::test]
async fn password_change_revokes_old_password() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let admin_id = "00000000-0000-0000-0000-000000000001";
    // 改密（管理员改自己）。
    let response = send(
        &app,
        "PATCH",
        &format!("/api/v1/users/{admin_id}"),
        Some("management-secret"),
        json!({ "password": "new-secret" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    // 旧密码立即失效。
    let old = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        None,
        json!({ "username": "admin", "password": "management-secret" }),
    )
    .await;
    assert_eq!(old.status(), StatusCode::UNAUTHORIZED);
    // 新密码可登录，且签发的会话 token 可用。
    let fresh = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        None,
        json!({ "username": "admin", "password": "new-secret" }),
    )
    .await;
    assert_eq!(fresh.status(), StatusCode::OK);
    let body = json_body(fresh).await;
    let token = body["data"]["token"].as_str().unwrap().to_string();
    let me = send(&app, "GET", "/api/v1/auth/me", Some(&token), Value::Null).await;
    assert_eq!(me.status(), StatusCode::OK);
}

#[tokio::test]
async fn same_password_for_two_users_does_not_clobber_login() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    for login in ["alice", "bob"] {
        let response = send(
            &app,
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": login, "password": "shared-pass" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED, "{login} create");
    }
    // 两个用户用相同密码都能登录（旧模型 INSERT OR REPLACE 会覆盖归属）。
    for login in ["alice", "bob"] {
        let response = send(
            &app,
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": login, "password": "shared-pass" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "{login} login");
        let body = json_body(response).await;
        let token = body["data"]["token"].as_str().unwrap().to_string();
        let me = send(&app, "GET", "/api/v1/auth/me", Some(&token), Value::Null).await;
        let me_body = json_body(me).await;
        assert_eq!(me_body["data"]["login"], login);
    }
}

#[tokio::test]
async fn member_cannot_manage_users() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    // 建一个成员。
    let created = send(
        &app,
        "POST",
        "/api/v1/users",
        Some("management-secret"),
        json!({ "login": "member1", "password": "member-pass" }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let member_login = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        None,
        json!({ "username": "member1", "password": "member-pass" }),
    )
    .await;
    let member_body = json_body(member_login).await;
    let member_token = member_body["data"]["token"].as_str().unwrap().to_string();
    let admin_id = "00000000-0000-0000-0000-000000000001";
    // 成员改管理员密码 → 403（旧代码只验登录不验角色，会 200）。
    let patch = send(
        &app,
        "PATCH",
        &format!("/api/v1/users/{admin_id}"),
        Some(&member_token),
        json!({ "password": "hijacked" }),
    )
    .await;
    assert_eq!(patch.status(), StatusCode::FORBIDDEN);
    // 成员列用户 → 403。
    let list = send(
        &app,
        "GET",
        "/api/v1/users",
        Some(&member_token),
        Value::Null,
    )
    .await;
    assert_eq!(list.status(), StatusCode::FORBIDDEN);
    // 管理员密码未被改。
    let admin_login = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        None,
        json!({ "username": "admin", "password": "management-secret" }),
    )
    .await;
    assert_eq!(admin_login.status(), StatusCode::OK);
}
