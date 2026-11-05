use super::*;

fn set_subtitle(f: &Fixture, name: &str, bytes: &[u8]) {
    let path = f._tmp.path().join(name);
    std::fs::write(&path, bytes).unwrap();
}

#[tokio::test]
async fn subtitle_session_rejects_another_user_and_closed_session() {
    for prefix in ["", "/emby"] {
        let mut f = Fixture::new();
        set_subtitle(&f, "matrix.srt", b"subtitle A");
        let info = f.info(prefix).await;
        let sid = info["PlaySessionId"].as_str().unwrap();
        let source = info["MediaSources"][0]["Id"].as_str().unwrap();
        let url = info["MediaSources"][0]["MediaStreams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|track| track["Type"] == "Subtitle")
            .unwrap()["DeliveryUrl"]
            .as_str()
            .unwrap();
        let target = format!("{prefix}{url}&PlaySessionId={sid}");
        f.event(
            prefix,
            "/Sessions/Playing",
            Some(source),
            Some(sid),
            10_000_000,
        )
        .await;
        f.event(
            prefix,
            "/Sessions/Playing/Stopped",
            Some(source),
            Some(sid),
            20_000_000,
        )
        .await;
        let response = f
            .app
            .clone()
            .oneshot(
                Request::get(target.as_str())
                    .header("authorization", "Bearer admin-token")
                    .header("X-Emby-Device-Id", DEVICE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let info = f.info(prefix).await;
        let sid = info["PlaySessionId"].as_str().unwrap();
        f.use_member().await;
        let response = f
            .app
            .clone()
            .oneshot(
                Request::get(format!("{prefix}{url}&PlaySessionId={sid}"))
                    .header("authorization", format!("Bearer {}", f.token))
                    .header("X-Emby-Device-Id", DEVICE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn hidden_library_subtitle_is_not_delivered() {
    for prefix in ["", "/emby"] {
        let mut f = Fixture::new();
        set_subtitle(&f, "matrix.srt", b"subtitle A");
        f.use_member().await;
        let info = f.info(prefix).await;
        let sid = info["PlaySessionId"].as_str().unwrap();
        let url = info["MediaSources"][0]["MediaStreams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|track| track["Type"] == "Subtitle")
            .unwrap()["DeliveryUrl"]
            .as_str()
            .unwrap();
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
        for suffix in [String::new(), format!("&PlaySessionId={sid}")] {
            let response = f
                .app
                .clone()
                .oneshot(
                    Request::get(format!("{prefix}{url}{suffix}"))
                        .header("authorization", format!("Bearer {}", f.token))
                        .header("X-Emby-Device-Id", DEVICE)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }
}

#[tokio::test]
async fn old_session_cannot_deliver_after_library_configuration_changes() {
    for prefix in ["", "/emby"] {
        let f = Fixture::new();
        set_subtitle(&f, "matrix.srt", b"subtitle A");
        let info = f.info(prefix).await;
        let sid = info["PlaySessionId"].as_str().unwrap();
        let source = info["MediaSources"][0]["Id"].as_str().unwrap();
        let url = info["MediaSources"][0]["MediaStreams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|track| track["Type"] == "Subtitle")
            .unwrap()["DeliveryUrl"]
            .as_str()
            .unwrap();
        let file = f
            .store
            .file_coverage_by_path(&f._tmp.path().join("matrix.mkv").display().to_string())
            .unwrap()
            .unwrap()
            .file;
        f.store
            .begin_library_integrity(file.library_id, false, "config-change", 5000)
            .unwrap();
        let mut finished = false;
        for step in 1..1000 {
            if f.store
                .advance_library_operation("config-change", step, 1)
                .unwrap()
                .state
                == "succeeded"
            {
                finished = true;
                break;
            }
        }
        assert!(finished);
        for target in [
            format!("{prefix}{url}&PlaySessionId={sid}"),
            format!(
                "{prefix}/Videos/{}/stream?MediaSourceId={source}&PlaySessionId={sid}",
                f.item
            ),
        ] {
            let response = f
                .app
                .clone()
                .oneshot(
                    Request::get(target)
                        .header("authorization", "Bearer admin-token")
                        .header("X-Emby-Device-Id", DEVICE)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        let response = f
            .app
            .clone()
            .oneshot(
                Request::get(format!("{prefix}{url}"))
                    .header("authorization", "Bearer admin-token")
                    .header("X-Emby-Device-Id", DEVICE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
            b"subtitle A"
        );
        let fresh = f.info(prefix).await;
        let fresh_sid = fresh["PlaySessionId"].as_str().unwrap();
        let response = f
            .app
            .clone()
            .oneshot(
                Request::get(format!("{prefix}{url}&PlaySessionId={fresh_sid}"))
                    .header("authorization", "Bearer admin-token")
                    .header("X-Emby-Device-Id", DEVICE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
            b"subtitle A"
        );
    }
}

async fn detail_subtitle_urls(f: &Fixture, prefix: &str) -> [String; 2] {
    let detail = f
        .app
        .clone()
        .oneshot(
            Request::get(format!("{prefix}/Items/{}", f.item))
                .header("authorization", "Bearer admin-token")
                .header("X-Emby-Device-Id", DEVICE)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let detail = json(detail).await;
    [
        &detail["MediaStreams"],
        &detail["MediaSources"][0]["MediaStreams"],
    ]
    .map(|streams| {
        streams
            .as_array()
            .unwrap()
            .iter()
            .find(|track| track["Type"] == "Subtitle")
            .unwrap()["DeliveryUrl"]
            .as_str()
            .unwrap()
            .to_owned()
    })
}

#[tokio::test]
async fn subtitle_delivery_stays_with_advertised_source_after_preference_changes() {
    for prefix in ["", "/emby"] {
        let f = Fixture::new();
        set_subtitle(&f, "matrix.srt", b"subtitle A");
        let info = f.info(prefix).await;
        let detail_urls = detail_subtitle_urls(&f, prefix).await;
        let sid = info["PlaySessionId"].as_str().unwrap();
        let url = info["MediaSources"][0]["MediaStreams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|track| track["Type"] == "Subtitle")
            .unwrap()["DeliveryUrl"]
            .as_str()
            .unwrap()
            .to_string();
        f.publish_new_preferred_source();
        set_subtitle(&f, "new-preferred.srt", b"subtitle B");
        for detail_url in detail_urls {
            let response = f
                .app
                .clone()
                .oneshot(
                    Request::get(format!("{prefix}{detail_url}"))
                        .header("authorization", "Bearer admin-token")
                        .header("X-Emby-Device-Id", DEVICE)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
                b"subtitle A"
            );
        }
        let response = f
            .app
            .clone()
            .oneshot(
                Request::get(format!("{prefix}{url}"))
                    .header("authorization", "Bearer admin-token")
                    .header("X-Emby-Device-Id", DEVICE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
            b"subtitle A"
        );
        let separator = if url.contains('?') { '&' } else { '?' };
        let response = f
            .app
            .clone()
            .oneshot(
                Request::get(format!("{prefix}{url}{separator}PlaySessionId={sid}"))
                    .header("authorization", "Bearer admin-token")
                    .header("X-Emby-Device-Id", DEVICE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
            b"subtitle A"
        );
        for (target, device) in [
            (
                format!("{prefix}{url}{separator}PlaySessionId={sid}"),
                "wrong-device",
            ),
            (
                format!("{prefix}/Videos/{}/Subtitles/0/Stream.srt", f.item),
                DEVICE,
            ),
        ] {
            let response = f
                .app
                .clone()
                .oneshot(
                    Request::get(target)
                        .header("authorization", "Bearer admin-token")
                        .header("X-Emby-Device-Id", device)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        f.replace_source();
        let response = f
            .app
            .clone()
            .oneshot(
                Request::get(format!("{prefix}{url}"))
                    .header("authorization", "Bearer admin-token")
                    .header("X-Emby-Device-Id", DEVICE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
