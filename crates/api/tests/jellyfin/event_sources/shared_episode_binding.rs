use super::*;

struct EpisodeFixture {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    store: Store,
    user: domain::UserId,
    media: MediaId,
    source: domain::PhysicalSourceId,
    root: PathBuf,
}

#[tokio::test]
async fn shared_file_subtitle_session_rejects_another_episode() {
    for prefix in ["", "/emby"] {
        let f = EpisodeFixture::new();
        std::fs::write(f.root.join("a.srt"), b"episode subtitle").unwrap();
        let first = unit_id(f.media, 1, 1);
        let second = unit_id(f.media, 1, 2);
        let response = f
            .app
            .clone()
            .oneshot(
                Request::post(format!("{prefix}/Items/{first}/PlaybackInfo"))
                    .header("authorization", "Bearer admin-token")
                    .header("X-Emby-Device-Id", DEVICE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let info = json(response).await;
        let sid = info["PlaySessionId"].as_str().unwrap();
        let url = info["MediaSources"][0]["MediaStreams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|track| track["Type"] == "Subtitle")
            .unwrap()["DeliveryUrl"]
            .as_str()
            .unwrap();
        for (item, expected) in [(&first, StatusCode::OK), (&second, StatusCode::NOT_FOUND)] {
            let target = url.replacen(&first, item, 1);
            let response = f
                .app
                .clone()
                .oneshot(
                    Request::get(format!("{prefix}{target}&PlaySessionId={sid}"))
                        .header("authorization", "Bearer admin-token")
                        .header("X-Emby-Device-Id", DEVICE)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            if expected == StatusCode::OK {
                assert_eq!(
                    to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
                    b"episode subtitle"
                );
            }
        }
    }
}

impl EpisodeFixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let (app, _, _) = app(tmp.path());
        let store = Store::open(tmp.path().join("data")).unwrap();
        let root = tmp.path().join("shared-tv");
        std::fs::create_dir_all(&root).unwrap();
        store
            .create_library(
                MediaKind::Tv,
                "Shared TV",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media = catalog::new_media(MediaKind::Tv, "Shared episode file");
        store.insert_media(&media).unwrap();
        let path = root.join("a.mkv");
        std::fs::write(&path, b"AAAAAAAA").unwrap();
        let file = store::test_support::publish_fixture_file_units(
            &store,
            &FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                ..Default::default()
            },
            &[domain::UnitKey::new(1, 1), domain::UnitKey::new(1, 2)],
        )
        .unwrap();
        let user = store.user_id_by_token("admin-token").unwrap().unwrap();
        Self {
            _tmp: tmp,
            app,
            store,
            user,
            media: media.id,
            source: domain::PhysicalSourceId {
                file_id: file.file_id,
                revision: file.version.revision,
            },
            root,
        }
    }

    fn publish_preferred(&self) {
        let path = self.root.join("b.mkv");
        std::fs::write(&path, b"BBBBBBBB").unwrap();
        let file = store::test_support::publish_fixture_file_units(
            &self.store,
            &FixtureFile {
                media_id: self.media,
                path: path.display().to_string(),
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                ..Default::default()
            },
            &[domain::UnitKey::new(1, 1), domain::UnitKey::new(1, 2)],
        )
        .unwrap();
        let scope = store::library_write::WriteScope {
            library_id: file.library_id,
            config_revision: self
                .store
                .library_config_revision(&file.library_id.to_string())
                .unwrap(),
        };
        std::fs::write(&path, b"BBBBBBBB revised").unwrap();
        let observation = api::fingerprint_job::observe_source(&path).unwrap();
        self.store
            .replace_file_source(&scope, file.file_id, &file.version, &observation, 5_000)
            .unwrap();
        for episode in [1, 2] {
            let item =
                domain::TypedItemId::from_wire_string(&unit_id(self.media, 1, episode)).unwrap();
            let entries = self.store.playable_entries(self.user, &item).unwrap();
            assert_eq!(entries[0].file.file_id, file.file_id);
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.file.file_id == self.source.file_id
                        && entry.file.version.revision == self.source.revision)
            );
        }
    }

    async fn event(&self, prefix: &str, path: &str, item: &str, sid: &str, ticks: i64) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::post(format!("{prefix}{path}"))
                    .header("authorization", "Bearer admin-token")
                    .header("X-Emby-Device-Id", DEVICE)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"ItemId":item,"PlaySessionId":sid,"PositionTicks":ticks})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    async fn stream(&self, prefix: &str, item: &str, sid: &str) -> (StatusCode, Vec<u8>) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::get(format!(
                    "{prefix}/Videos/{item}/stream?PlaySessionId={sid}&DeviceId={DEVICE}"
                ))
                .header("authorization", "Bearer admin-token")
                .header("X-Emby-Device-Id", DEVICE)
                .header("range", "bytes=0-3")
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        (
            status,
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
    }
}

#[tokio::test]
async fn shared_file_session_stays_bound_to_episode_after_preference_changes() {
    for prefix in ["", "/emby"] {
        let f = EpisodeFixture::new();
        let first = unit_id(f.media, 1, 1);
        let second = unit_id(f.media, 1, 2);
        let response = f
            .app
            .clone()
            .oneshot(
                Request::post(format!("{prefix}/Items/{first}/PlaybackInfo"))
                    .header("authorization", "Bearer admin-token")
                    .header("X-Emby-Device-Id", DEVICE)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let info = json(response).await;
        let sid = info["PlaySessionId"].as_str().unwrap();
        assert_eq!(info["MediaSources"][0]["Id"], f.source.to_wire_string());
        f.publish_preferred();
        let (status, bytes) = f.stream(prefix, &first, sid).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(bytes, b"AAAA");
        f.event(prefix, "/Sessions/Playing", &first, sid, 10_000_000)
            .await;
        let active = f.store.get_session(f.user, DEVICE).unwrap().unwrap();
        assert_eq!(active.ledger_id, Some(f.source.file_id.to_string()));
        let first_before = f.store.unit_state(f.user, f.media, 1, 1).unwrap();
        let logs_before = f
            .store
            .list_logs(100, None, None, None, 0, Some(f.user))
            .unwrap();
        for path in [
            "/Sessions/Playing",
            "/Sessions/Playing/Progress",
            "/Sessions/Playing/Stopped",
        ] {
            f.event(prefix, path, &second, sid, 90_000_000).await;
            assert_eq!(
                f.store.get_session(f.user, DEVICE).unwrap(),
                Some(active.clone())
            );
            assert_eq!(
                f.store.unit_state(f.user, f.media, 1, 1).unwrap(),
                first_before
            );
            assert!(f.store.unit_state(f.user, f.media, 1, 2).unwrap().is_none());
            assert_eq!(
                f.store
                    .list_logs(100, None, None, None, 0, Some(f.user))
                    .unwrap(),
                logs_before
            );
        }
        let (status, bytes) = f.stream(prefix, &second, sid).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_ne!(bytes, b"AAAA");
        f.event(
            prefix,
            "/Sessions/Playing/Progress",
            &first,
            sid,
            30_000_000,
        )
        .await;
        f.event(prefix, "/Sessions/Playing/Stopped", &first, sid, 50_000_000)
            .await;
        assert!(f.store.get_session(f.user, DEVICE).unwrap().is_none());
        assert_eq!(
            f.store
                .unit_state(f.user, f.media, 1, 1)
                .unwrap()
                .unwrap()
                .position_ms,
            5_000
        );
        assert!(f.store.unit_state(f.user, f.media, 1, 2).unwrap().is_none());
        let logs = f
            .store
            .list_logs(100, None, None, None, 0, Some(f.user))
            .unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].end_position_ms, 5_000);
        assert_eq!(
            f.store
                .play_session_binding(f.user, DEVICE, sid)
                .unwrap()
                .unwrap()
                .state,
            store::PlaySessionIdState::Closed
        );
    }
}
