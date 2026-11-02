use super::*;

struct ChapterGate {
    entered: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    released: parking_lot::Mutex<bool>,
    changed: parking_lot::Condvar,
}

impl api::marker_resolver::ChapterProbe for ChapterGate {
    fn probe(&self, _: &std::path::Path) -> Option<Vec<marker::Chapter>> {
        if let Some(entered) = self.entered.lock().take() {
            let _ = entered.send(());
        }
        let mut released = self.released.lock();
        self.changed.wait_while_for(
            &mut released,
            |released| !*released,
            Duration::from_secs(10),
        );
        assert!(*released, "test must release the chapter probe");
        Some(Vec::new())
    }
}

struct ReleaseChapters(Arc<ChapterGate>);
impl Drop for ReleaseChapters {
    fn drop(&mut self) {
        *self.0.released.lock() = true;
        self.0.changed.notify_all();
    }
}

#[test]
fn background_chapter_probe_cannot_reject_foreground_playback_session() {
    let mut f = Fixture::new();
    let (entered, started) = tokio::sync::oneshot::channel();
    let gate = Arc::new(ChapterGate {
        entered: parking_lot::Mutex::new(Some(entered)),
        released: parking_lot::Mutex::new(false),
        changed: parking_lot::Condvar::new(),
    });
    let release = ReleaseChapters(gate.clone());
    f.state = f
        .state
        .with_http_work(Arc::new(api::blocking_budget::BlockingBudget::new(1)))
        .with_chapter_probe(gate);
    let store = f.state.store();
    let row = store.file_coverage_by_id(f.file.file_id).unwrap().unwrap();
    let media = store.get_media(f.file.media_id).unwrap().unwrap();
    runtime().block_on(async {
        let state = f.state.clone();
        let background = tokio::spawn(async move {
            api::marker_resolver::resolve_item_chapters(
                &state,
                &row,
                domain::UnitKey::WHOLE,
                &media,
                None,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(3), started)
            .await
            .expect("chapter probe should enter")
            .unwrap();
        let response = router(f.state.clone())
            .oneshot(
                Request::post("/api/v1/playback/sessions")
                    .header("authorization", format!("Bearer {}", f.token))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "media_item_id": f.file.media_id,
                            "device_id": "foreground-playback"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let session_status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let session: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let heartbeat = router(f.state.clone())
            .oneshot(
                Request::post("/api/v1/playback/progress")
                    .header("authorization", format!("Bearer {}", f.token))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "media_item_id": f.file.media_id,
                            "file_id": session["data"]["file_id"],
                            "play_session_id": session["data"]["play_session_id"],
                            "device_id": "foreground-playback",
                            "event": "start", "event_sequence": 1,
                            "position_ms": 0, "duration_ms": 120_000
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        drop(release);
        background.await.unwrap();
        assert_eq!(session_status, StatusCode::OK);
        assert_eq!(heartbeat.status(), StatusCode::OK);
        assert!(
            store
                .get_cached_chapters(&f.file.file_id.to_string())
                .unwrap()
                .is_some()
        );
    });
}
