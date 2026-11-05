use super::*;
use std::path::Path;
use std::sync::Arc;

struct PublishOnRead(parking_lot::Mutex<Option<Box<dyn FnOnce() + Send>>>);

impl api::file_stat_read::FileStatRead for PublishOnRead {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        library::read_nfo(path)
    }
    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
    fn external_subtitles(&self, path: &Path) -> Result<Vec<library::SubtitleTrack>, String> {
        let hook = self.0.lock().take();
        if let Some(hook) = hook {
            hook();
        }
        library::external_subtitle_tracks(path).map_err(|error| error.to_string())
    }
}

#[tokio::test]
async fn playback_info_session_matches_snapshot_when_preference_changes() {
    for prefix in ["", "/emby"] {
        let f = Fixture::new();
        let store = Store::open(f._tmp.path().join("data")).unwrap();
        let root = f._tmp.path().to_path_buf();
        let media = f.media;
        let files = Arc::new(PublishOnRead(parking_lot::Mutex::new(Some(Box::new(
            move || {
                let path = root.join("race.mkv");
                std::fs::write(&path, b"new source").unwrap();
                let file = store::test_support::publish_fixture_file(
                    &store,
                    &FixtureFile {
                        media_id: media,
                        path: path.display().to_string(),
                        quality_source: QualitySource::Probe,
                        confidence: Confidence::High,
                        ..Default::default()
                    },
                )
                .unwrap();
                let scope = store::library_write::WriteScope {
                    library_id: file.library_id,
                    config_revision: store
                        .library_config_revision(&file.library_id.to_string())
                        .unwrap(),
                };
                std::fs::write(&path, b"new source revised").unwrap();
                let observation = api::fingerprint_job::observe_source(&path).unwrap();
                store
                    .replace_file_source(&scope, file.file_id, &file.version, &observation, 5000)
                    .unwrap();
            },
        )))));
        let state = ApiState::new(
            Store::open(f._tmp.path().join("data")).unwrap(),
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(f._tmp.path().join("stage"))),
        )
        .unwrap()
        .with_file_stat_read(files);
        let app = router(state);
        let response = app
            .clone()
            .oneshot(
                Request::post(format!("{prefix}/Items/{}/PlaybackInfo", f.item))
                    .header("authorization", "Bearer admin-token")
                    .header("X-Emby-Device-Id", DEVICE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let info = json(response).await;
        let source = info["MediaSources"][0]["Id"].as_str().unwrap();
        let sid = info["PlaySessionId"].as_str().unwrap();
        assert_eq!(source, f.source.to_wire_string());
        let item = domain::TypedItemId::from_wire_string(&f.item).unwrap();
        assert_ne!(
            f.store.playable_entries(f.user, &item).unwrap()[0]
                .file
                .file_id,
            f.source.file_id
        );
        let response = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "{prefix}/Videos/{}/stream?MediaSourceId={source}&PlaySessionId={sid}",
                    f.item
                ))
                .header("authorization", "Bearer admin-token")
                .header("X-Emby-Device-Id", DEVICE)
                .header("range", "bytes=0-3")
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
            b"0123"
        );
        let response = app.oneshot(Request::post(format!("{prefix}/Sessions/Playing"))
            .header("authorization", "Bearer admin-token").header("X-Emby-Device-Id", DEVICE)
            .header("content-type", "application/json")
            .body(Body::from(json!({"ItemId": f.item, "MediaSourceId": source, "PlaySessionId": sid, "PositionTicks": 30000000}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(f.state().1.unwrap().position_ms, 3000);
    }
}

#[tokio::test]
async fn playback_info_rejects_snapshot_replaced_during_rendering() {
    for prefix in ["", "/emby"] {
        let f = Fixture::new();
        let store = Store::open(f._tmp.path().join("data")).unwrap();
        let path = f._tmp.path().join("matrix.mkv");
        let files = Arc::new(PublishOnRead(parking_lot::Mutex::new(Some(Box::new(
            move || {
                let file = store
                    .file_coverage_by_path(&path.display().to_string())
                    .unwrap()
                    .unwrap()
                    .file;
                let scope = store::library_write::WriteScope {
                    library_id: file.library_id,
                    config_revision: store
                        .library_config_revision(&file.library_id.to_string())
                        .unwrap(),
                };
                std::fs::write(&path, b"replacement source").unwrap();
                let observation = api::fingerprint_job::observe_source(&path).unwrap();
                store
                    .replace_file_source(&scope, file.file_id, &file.version, &observation, 5000)
                    .unwrap();
            },
        )))));
        let state = ApiState::new(
            Store::open(f._tmp.path().join("data")).unwrap(),
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(f._tmp.path().join("stage"))),
        )
        .unwrap()
        .with_file_stat_read(files);
        let response = router(state)
            .oneshot(
                Request::post(format!("{prefix}/Items/{}/PlaybackInfo", f.item))
                    .header("authorization", "Bearer admin-token")
                    .header("X-Emby-Device-Id", DEVICE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::OK);
        assert_eq!(f.state(), (None, None, Vec::new()));
    }
}

#[tokio::test]
async fn playback_info_rejects_library_hidden_during_rendering() {
    for prefix in ["", "/emby"] {
        let mut f = Fixture::new();
        f.use_member().await;
        let store = Store::open(f._tmp.path().join("data")).unwrap();
        let file = store
            .file_coverage_by_path(&f._tmp.path().join("matrix.mkv").display().to_string())
            .unwrap()
            .unwrap()
            .file;
        let files = Arc::new(PublishOnRead(parking_lot::Mutex::new(Some(Box::new(
            move || {
                store
                    .update_library(
                        &file.library_id.to_string(),
                        None,
                        None,
                        Some(("selected", true, &[])),
                    )
                    .unwrap();
            },
        )))));
        let state = ApiState::new(
            Store::open(f._tmp.path().join("data")).unwrap(),
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(f._tmp.path().join("stage"))),
        )
        .unwrap()
        .with_file_stat_read(files);
        let response = router(state)
            .oneshot(
                Request::post(format!("{prefix}/Items/{}/PlaybackInfo", f.item))
                    .header("authorization", format!("Bearer {}", f.token))
                    .header("X-Emby-Device-Id", DEVICE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::OK);
        assert_eq!(f.state(), (None, None, Vec::new()));
    }
}
