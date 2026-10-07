use super::*;

#[tokio::test]
async fn authenticate_by_name_issues_token() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/Users/AuthenticateByName")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "Username": "admin", "Pw": "admin-token" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    // 新模型：登录签发会话 token（不再是密码本身）；用它调受保护接口应通过。
    let token = body["AccessToken"].as_str().unwrap().to_string();
    assert!(!token.is_empty() && token != "admin-token");
    let authed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/UserViews")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(authed.status(), StatusCode::OK);
}

#[tokio::test]
async fn jellyfin_token_header_variants_authenticate_library_requests() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    for (name, value) in [
        ("X-Emby-Token", "admin-token"),
        ("X-MediaBrowser-Token", "admin-token"),
        (
            "Authorization",
            "MediaBrowser Client=\"Infuse\", Token=\"admin-token\"",
        ),
        (
            "X-Emby-Authorization",
            "MediaBrowser Client=\"VidHub\", Token=\"admin-token\"",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/Items")
                    .header(name, value)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{name}");
    }
    let denied = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/Items")
                .header("X-Emby-Token", "wrong")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
}
