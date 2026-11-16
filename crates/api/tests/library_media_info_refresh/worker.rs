use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

struct Tracks(AtomicBool);
impl api::probe_manager::TracksProbe for Tracks {
    fn probe(
        &self,
        _: &std::path::Path,
        _: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        if self.0.load(Ordering::SeqCst) {
            return Err(library::LibraryError::Probe(
                "source temporarily offline".into(),
            ));
        }
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack {
                    codec: Some("hevc".into()),
                    width: Some(3840),
                    height: Some(2160),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some(120_000),
        ))
    }
}

struct NoCapture;
impl marker::FingerprintCaptureEngine for NoCapture {
    fn capture_window(
        &self,
        _: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        Err(marker::CaptureFailure {
            kind: marker::CaptureFailureKind::InvalidWindow,
            message: "disabled fingerprint must not be read".into(),
            metrics: Default::default(),
        })
    }
}

async fn wait_for_job(store: &api::Store, id: &str, expected: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let job = store.get_probe_job(id).unwrap().unwrap();
            if !job.is_active() {
                assert_eq!(job.status, expected);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("media information job must finish");
}

#[test]
fn failed_media_information_can_be_retried_immediately_without_enabling_intros() {
    for (detect, fingerprint) in [(false, false), (true, false), (false, true)] {
        retry_with_settings(detect, fingerprint, "missing");
    }
}

#[test]
fn force_refresh_keeps_old_information_after_failure_and_replaces_it_after_success() {
    retry_with_settings(false, false, "force");
}

fn retry_with_settings(detect: bool, fingerprint: bool, mode: &str) {
    let mut fixture = Fixture::new();
    let file = fixture.file(1);
    if mode == "force" {
        fixture.cache(&file, Some(60_000));
    }
    let original = fixture
        .state
        .store()
        .get_file_meta(&file.file_id.to_string())
        .unwrap();
    fixture
        .state
        .store()
        .set_library_intro_settings(&fixture.library, detect, fingerprint)
        .unwrap();
    let tracks = Arc::new(Tracks(AtomicBool::new(true)));
    fixture.state = fixture
        .state
        .with_probe_engines(
            Arc::new(marker::ChromaprintEngine),
            Arc::new(NoCapture),
            tracks.clone(),
        )
        .unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        assert!(fixture.state.probe.try_start_workers());
        let store = fixture.state.store();
        let app = router(fixture.state.clone());
        let uri = format!("/api/v1/libraries/{}/media-info/refresh", fixture.library);
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &uri,
                Some("management-secret"),
                json!({ "mode": mode, "request_id": uuid::Uuid::new_v4().to_string() }),
            ))
            .await
            .unwrap();
        assert_eq!(json_body(response).await["data"]["queued"], 1);
        let failed = store
            .latest_probe_job_for_ledger(&file.file_id.to_string())
            .unwrap()
            .unwrap();
        wait_for_job(&store, &failed, "failed").await;
        assert_eq!(
            store.get_file_meta(&file.file_id.to_string()).unwrap(),
            original
        );
        tracks.0.store(false, Ordering::SeqCst);
        let response = app
            .oneshot(request(
                "POST",
                &uri,
                Some("management-secret"),
                json!({ "mode": mode, "request_id": uuid::Uuid::new_v4().to_string() }),
            ))
            .await
            .unwrap();
        assert_eq!(
            json_body(response).await["data"]["queued"],
            1,
            "manual retry must bypass recent failure backoff"
        );
        let retried = store
            .latest_probe_job_for_ledger(&file.file_id.to_string())
            .unwrap()
            .unwrap();
        assert_ne!(retried, failed);
        wait_for_job(&store, &retried, "succeeded").await;
        assert_eq!(
            store
                .latest_probe_job_for_ledger(&file.file_id.to_string())
                .unwrap()
                .unwrap(),
            retried,
            "disabled optional work must not create a successor job"
        );
        let cached = store
            .get_file_meta(&file.file_id.to_string())
            .unwrap()
            .unwrap();
        assert_eq!(cached.video.unwrap().codec.as_deref(), Some("hevc"));
        assert!(
            store
                .get_fingerprint_cache(&file.file_id.to_string())
                .unwrap()
                .is_none()
        );
        fixture.state.probe.shutdown_workers().await.unwrap();
    });
}
