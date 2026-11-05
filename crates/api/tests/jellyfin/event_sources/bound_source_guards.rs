use super::*;

async fn send(
    f: &Fixture,
    prefix: &str,
    path: &str,
    token: &str,
    device: &str,
    item: &str,
    sid: &str,
) -> StatusCode {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::post(format!("{prefix}{path}"))
                .header("authorization", format!("Bearer {token}"))
                .header("X-Emby-Device-Id", device)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"ItemId": item, "PlaySessionId": sid, "PositionTicks": 50_000_000})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    response.status()
}

async fn stream(
    f: &Fixture,
    prefix: &str,
    token: &str,
    device: &str,
    item: &str,
    sid: &str,
) -> StatusCode {
    f.app
        .clone()
        .oneshot(
            Request::get(format!(
                "{prefix}/Videos/{item}/stream?PlaySessionId={sid}&DeviceId={device}"
            ))
            .header("authorization", format!("Bearer {token}"))
            .header("X-Emby-Device-Id", device)
            .header("range", "bytes=0-3")
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

fn other_item(f: &Fixture) -> (MediaId, String) {
    let media = catalog::new_media(MediaKind::Movie, "Other item");
    f.store.insert_media(&media).unwrap();
    let path = f._tmp.path().join("other-bound-item.mkv");
    std::fs::write(&path, b"other content").unwrap();
    store::test_support::publish_fixture_file(
        &f.store,
        &FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            ..Default::default()
        },
    )
    .unwrap();
    (
        media.id,
        domain::TypedItemId::Work(media.id).to_wire_string(),
    )
}

#[tokio::test]
async fn unspecified_source_rejects_wrong_identity_target_and_closed_session() {
    for prefix in ["", "/emby"] {
        let mut f = Fixture::new();
        let info = f.info(prefix).await;
        let sid = info["PlaySessionId"].as_str().unwrap().to_string();
        let (other_media, other) = other_item(&f);
        f.use_member().await;
        let member_before = f.state();
        let admin = f.store.user_id_by_token("admin-token").unwrap().unwrap();
        let admin_before = f.store.get_session(admin, DEVICE).unwrap();
        let unit_before = f.store.unit_state(admin, f.media, -1, -1).unwrap();
        let logs_before = f
            .store
            .list_logs(100, None, None, None, 0, Some(admin))
            .unwrap();
        for (token, device, item, session) in [
            ("admin-token", DEVICE, f.item.as_str(), "unknown"),
            ("admin-token", "wrong-device", f.item.as_str(), sid.as_str()),
            (f.token.as_str(), DEVICE, f.item.as_str(), sid.as_str()),
            ("admin-token", DEVICE, other.as_str(), sid.as_str()),
        ] {
            for path in [
                "/Sessions/Playing",
                "/Sessions/Playing/Progress",
                "/Sessions/Playing/Stopped",
            ] {
                assert_eq!(
                    send(&f, prefix, path, token, device, item, session).await,
                    StatusCode::NO_CONTENT
                );
            }
            assert_eq!(
                stream(&f, prefix, token, device, item, session).await,
                StatusCode::NOT_FOUND
            );
            assert_eq!(f.state(), member_before);
            assert_eq!(f.store.get_session(admin, DEVICE).unwrap(), admin_before);
            assert!(
                f.store
                    .get_session(admin, "wrong-device")
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                f.store.unit_state(admin, f.media, -1, -1).unwrap(),
                unit_before
            );
            assert_eq!(
                f.store
                    .list_logs(100, None, None, None, 0, Some(admin))
                    .unwrap(),
                logs_before
            );
            assert!(
                f.store
                    .unit_state(admin, other_media, -1, -1)
                    .unwrap()
                    .is_none()
            );
        }
        f.token = "admin-token".into();
        f.user = admin;
        f.event(prefix, "/Sessions/Playing", None, Some(&sid), 10_000_000)
            .await;
        f.event(
            prefix,
            "/Sessions/Playing/Stopped",
            None,
            Some(&sid),
            20_000_000,
        )
        .await;
        let closed = f.state();
        assert!(closed.0.is_none());
        assert_eq!(closed.1.as_ref().unwrap().position_ms, 2_000);
        assert_eq!(closed.2.len(), 1);
        for path in [
            "/Sessions/Playing",
            "/Sessions/Playing/Progress",
            "/Sessions/Playing/Stopped",
        ] {
            assert_eq!(
                send(&f, prefix, path, "admin-token", DEVICE, &f.item, &sid).await,
                StatusCode::NO_CONTENT
            );
            assert_eq!(f.state(), closed);
        }
        assert_eq!(
            stream(&f, prefix, "admin-token", DEVICE, &f.item, &sid).await,
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn unspecified_source_rejects_session_after_library_is_hidden() {
    for prefix in ["", "/emby"] {
        let mut f = Fixture::new();
        f.use_member().await;
        let info = f.info(prefix).await;
        let sid = info["PlaySessionId"].as_str().unwrap().to_string();
        let file = f
            .store
            .file_coverage_by_path(&f._tmp.path().join("matrix.mkv").display().to_string())
            .unwrap()
            .unwrap()
            .file;
        f.store
            .update_library(
                &file.library_id.to_string(),
                None,
                None,
                Some(("selected", true, &[])),
            )
            .unwrap();
        let before = f.state();
        for path in [
            "/Sessions/Playing",
            "/Sessions/Playing/Progress",
            "/Sessions/Playing/Stopped",
        ] {
            assert_eq!(
                send(&f, prefix, path, &f.token, DEVICE, &f.item, &sid).await,
                StatusCode::NO_CONTENT
            );
            assert_eq!(f.state(), before);
        }
        assert_eq!(
            stream(&f, prefix, &f.token, DEVICE, &f.item, &sid).await,
            StatusCode::NOT_FOUND
        );
    }
}
