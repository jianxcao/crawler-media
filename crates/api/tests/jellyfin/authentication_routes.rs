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

    let token = issue_token(&app).await;
    for prefix in ["", "/emby"] {
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
