use super::*;

#[tokio::test]
async fn revoked_token_cannot_change_or_omit_device_and_other_tokens_survive() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    for token in ["bedroom-token", "living-token", "web-token"] {
        store.set_user_token(admin(), token).unwrap();
    }
    let stream = format!("/Videos/{id}/stream");
    for (token, device) in [("bedroom-token", "bedroom"), ("living-token", "living")] {
        assert_eq!(
            send(
                &app,
                "GET",
                &stream,
                &format!("Bearer {token}"),
                json!({}),
                Some(device)
            )
            .await
            .status(),
            StatusCode::OK
        );
    }
    let uri = format!("/api/v1/playback/devices/bedroom?user_id={}", admin());
    assert_eq!(
        send(&app, "DELETE", &uri, "Bearer admin-token", json!({}), None)
            .await
            .status(),
        StatusCode::OK
    );
    for device in [Some("bedroom"), Some("changed-device"), None] {
        assert_eq!(
            send(
                &app,
                "GET",
                &stream,
                "Bearer bedroom-token",
                json!({}),
                device
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        send(
            &app,
            "GET",
            &stream,
            "Bearer living-token",
            json!({}),
            Some("living")
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(store.user_id_by_token("web-token").unwrap(), Some(admin()));
    assert_eq!(
        store.user_id_by_token("admin-token").unwrap(),
        Some(admin())
    );
}

#[tokio::test]
async fn cli_only_device_returns_unsupported_and_cli_is_never_invalidated() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, id) = app(tmp.path());
    assert_eq!(
        send(
            &app,
            "POST",
            "/Sessions/Playing",
            "Bearer admin-token",
            json!({"ItemId":id}),
            Some("cli-device")
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    let uri = format!("/api/v1/playback/devices/cli-device?user_id={}", admin());
    let response = send(&app, "DELETE", &uri, "Bearer admin-token", json!({}), None).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(response).await["error"]["code"],
        "playback.unsupported"
    );
    let store = Store::open(tmp.path().join("data")).unwrap();
    assert!(!store.device_is_revocable(admin(), "cli-device").unwrap());
    assert_eq!(
        store.user_id_by_token("admin-token").unwrap(),
        Some(admin())
    );
}
