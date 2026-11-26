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

const ADMIN_ID: &str = "00000000-0000-0000-0000-000000000001";

async fn issue_token(app: &axum::Router) -> String {
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
    json(response).await["AccessToken"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn authenticate_by_name_returns_full_jellyfin_result_and_binds_device() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let pub_info = json(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Info/Public")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(pub_info["Version"], "10.9.11");
    let server_id = pub_info["Id"].as_str().unwrap().to_string();

    for (prefix, pw_key) in [("", "Pw"), ("/emby", "Password")] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("{prefix}/Users/AuthenticateByName"))
                    .header("content-type", "application/json")
                    .header(
                        "x-emby-authorization",
                        r#"MediaBrowser Client="VidHub", Device="MacStudio", DeviceId="vidhub-mac-1", Version="3.0.4""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", pw_key: "admin-token" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = json(res).await;
        assert_eq!(body["ServerId"], server_id);
        assert_eq!(body["User"]["Id"], ADMIN_ID);
        assert_eq!(body["User"]["Name"], "admin");
        assert_eq!(body["User"]["ServerId"], server_id);
        assert_eq!(body["User"]["HasPassword"], true);
        assert_eq!(body["User"]["HasConfiguredPassword"], true);
        assert_eq!(body["User"]["Policy"]["EnableMediaPlayback"], true);
        assert_eq!(
            body["User"]["Policy"]["EnableVideoPlaybackTranscoding"],
            false
        );
        assert_eq!(
            body["User"]["Policy"]["EnableAudioPlaybackTranscoding"],
            false
        );
        assert_eq!(body["User"]["Policy"]["EnablePlaybackRemuxing"], false);
        assert_eq!(body["SessionInfo"]["UserId"], ADMIN_ID);
        assert_eq!(body["SessionInfo"]["UserName"], "admin");
        assert_eq!(body["SessionInfo"]["ServerId"], server_id);
        assert_eq!(body["SessionInfo"]["DeviceId"], "vidhub-mac-1");
        assert_eq!(body["SessionInfo"]["Client"], "VidHub");
        assert_eq!(body["SessionInfo"]["DeviceName"], "MacStudio");
        assert_eq!(body["SessionInfo"]["IsActive"], true);
        assert!(
            body["SessionInfo"]["Id"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        );
    }
}

#[tokio::test]
async fn authenticated_system_info_and_session_capabilities_endpoints() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());

    for prefix in ["", "/emby"] {
        let unauth = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{prefix}/System/Info"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);
    }

    for prefix in ["", "/emby"] {
        let token = issue_token(&app).await;
        let info_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{prefix}/System/Info"))
                    .header("authorization", format!("MediaBrowser Token=\"{token}\""))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(info_resp.status(), StatusCode::OK);
        let info = json(info_resp).await;
        assert_eq!(info["ProductName"], "crawler-media");
        assert_eq!(info["Version"], "10.9.11");
        assert_eq!(info["StartupWizardCompleted"], true);

        let endpoint_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{prefix}/System/Endpoint"))
                    .header("authorization", format!("MediaBrowser Token=\"{token}\""))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(endpoint_resp.status(), StatusCode::OK);

        for cap_uri in [
            format!("{prefix}/Sessions/Capabilities"),
            format!("{prefix}/Sessions/Capabilities/Full"),
            format!("{prefix}/Users/Logout"),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(cap_uri)
                        .header("authorization", format!("MediaBrowser Token=\"{token}\""))
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(
                resp.status().is_success(),
                "expected 2xx, got {}",
                resp.status()
            );
        }
    }
}

#[tokio::test]
async fn users_logout_revokes_the_token() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());

    for prefix in ["", "/emby"] {
        let token = issue_token(&app).await;
        let before = app
            .clone()
            .oneshot(
                Request::get(format!("{prefix}/System/Info"))
                    .header("authorization", format!("MediaBrowser Token=\"{token}\""))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(before.status(), StatusCode::OK, "{prefix} before logout");

        let logout = app
            .clone()
            .oneshot(
                Request::post(format!("{prefix}/Users/Logout"))
                    .header("authorization", format!("MediaBrowser Token=\"{token}\""))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout.status(), StatusCode::NO_CONTENT, "{prefix} logout");

        let after = app
            .clone()
            .oneshot(
                Request::get(format!("{prefix}/System/Info"))
                    .header("authorization", format!("MediaBrowser Token=\"{token}\""))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            after.status(),
            StatusCode::UNAUTHORIZED,
            "{prefix} after logout"
        );
    }
}

#[tokio::test]
async fn sessions_logout_revokes_the_token() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());

    for prefix in ["", "/emby"] {
        let token = issue_token(&app).await;
        let before = app
            .clone()
            .oneshot(
                Request::get(format!("{prefix}/System/Info"))
                    .header("authorization", format!("MediaBrowser Token=\"{token}\""))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(before.status(), StatusCode::OK, "{prefix} before logout");

        let logout = app
            .clone()
            .oneshot(
                Request::post(format!("{prefix}/Sessions/Logout"))
                    .header("authorization", format!("MediaBrowser Token=\"{token}\""))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout.status(), StatusCode::NO_CONTENT, "{prefix} logout");

        let after = app
            .clone()
            .oneshot(
                Request::get(format!("{prefix}/System/Info"))
                    .header("authorization", format!("MediaBrowser Token=\"{token}\""))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            after.status(),
            StatusCode::UNAUTHORIZED,
            "{prefix} after logout"
        );
    }
}

#[tokio::test]
async fn jellyfin_login_accepts_case_insensitive_property_names() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    for prefix in ["", "/emby"] {
        for (username_key, password_key) in [
            ("Username", "Pw"),
            ("username", "pw"),
            ("UserName", "PW"),
            ("USERNAME", "pW"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::post(format!("{prefix}/Users/AuthenticateByName"))
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({username_key: "admin", password_key: "admin-token"}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{prefix} {username_key} {password_key}"
            );
            let body = json(response).await;
            let token = body["AccessToken"].as_str().unwrap();
            let protected = app
                .clone()
                .oneshot(
                    Request::get(format!("{prefix}/UserViews"))
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(protected.status(), StatusCode::OK);
        }
    }
}

#[tokio::test]
async fn jellyfin_login_prefers_pw_when_password_is_also_present() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    for prefix in ["", "/emby"] {
        let response = app
            .clone()
            .oneshot(
                Request::post(format!("{prefix}/Users/AuthenticateByName"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"Username": "admin", "Pw": "admin-token", "Password": "incorrect"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{prefix}");
    }
}

#[tokio::test]
async fn jellyfin_login_rejects_invalid_credentials_and_malformed_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    for prefix in ["", "/emby"] {
        for (body, expected) in [
            (
                json!({"Username": "admin", "Pw": "wrong"}),
                StatusCode::UNAUTHORIZED,
            ),
            (
                json!({"Username": "admin", "Pw": "wrong", "Password": "admin-token"}),
                StatusCode::UNAUTHORIZED,
            ),
            (
                json!({"Username": "admin", "Pw": null, "Password": "admin-token"}),
                StatusCode::UNAUTHORIZED,
            ),
            (json!({"Username": "admin"}), StatusCode::UNAUTHORIZED),
            (
                json!({"Username": null, "Pw": "admin-token"}),
                StatusCode::UNAUTHORIZED,
            ),
            (
                json!({"Username": 42, "Pw": "admin-token"}),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                json!({"Username": "admin", "Pw": 42}),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::post(format!("{prefix}/Users/AuthenticateByName"))
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{prefix} {body}");
        }
    }
}

#[tokio::test]
async fn jellyfin_login_accepts_vidhub_password_and_ignores_extra_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    for prefix in ["", "/emby"] {
        let response = app.clone().oneshot(
            Request::post(format!("{prefix}/Users/AuthenticateByName"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"Username": "admin", "Password": "admin-token", "Extra": {"arbitrary": true}}).to_string()))
                .unwrap(),
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{prefix}");
    }
}

#[tokio::test]
async fn jellyfin_login_ignores_non_string_password_when_pw_is_present() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let response = app.oneshot(
        Request::post("/Users/AuthenticateByName")
            .header("content-type", "application/json")
            .body(Body::from(json!({"Username": "admin", "Pw": "admin-token", "Password": {"ignored": true}}).to_string()))
            .unwrap(),
    ).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
