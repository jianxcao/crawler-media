//! Metadata publication must dispatch replacement images after the service is idle.
use super::*;
use api::chapter_frame_service::{
    ChapterFrameRenderResult, ChapterFrameRequest, ChapterFrameRunner, ChapterFrameStaged,
};
use std::time::Duration;

struct DurationTracks;
impl api::probe_manager::TracksProbe for DurationTracks {
    fn probe(
        &self,
        _: &Path,
        _: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        Ok((library::Tracks::default(), Some(20_000)))
    }
}

struct Frames;
impl ChapterFrameRunner for Frames {
    fn render(
        &self,
        request: &ChapterFrameRequest,
        _: &library::WorkControl,
    ) -> Result<Vec<ChapterFrameRenderResult>, String> {
        Ok(request
            .candidates
            .iter()
            .map(|(index, artifact)| {
                let target = chapter_frame_artifact_path(
                    &request.video,
                    &request.generation,
                    *index,
                    artifact,
                );
                let staged = target.with_extension("staging.jpg");
                image::RgbImage::from_pixel(2, 2, image::Rgb([30, 60, 90]))
                    .save_with_format(&staged, image::ImageFormat::Jpeg)
                    .unwrap();
                ChapterFrameRenderResult {
                    index: *index,
                    staged: Ok(ChapterFrameStaged { staged, target }),
                }
            })
            .collect())
    }
}

fn fixture() -> Fixture {
    // The synchronous test constructs ApiState before entering a runtime, so
    // only the explicitly installed fake media engines can be started below.
    let mut f = Fixture::new(&[UnitKey::new(1, 1)]);
    for file in &f.files {
        f.store
            .put_cached_chapters(&file.file_id.to_string(), &[])
            .unwrap();
    }
    let file = &f.files[0];
    assert!(
        !f.store
            .get_library(&file.library_id.to_string())
            .unwrap()
            .unwrap()
            .enable_fingerprint
    );
    f.store
        .publish_probe_result(
            file.file_id,
            &store::library_write::probe::ProbePublication {
                expected: &file.version,
                tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
                resolution: None,
                codec: None,
                hdr: store::library_write::probe::Observed::NotObserved,
                duration_ms: Some(10_000),
                details: None,
            },
            1,
        )
        .unwrap();
    f.store
        .put_fixture_marker(&store::StoredFileMarker {
            file_id: file.file_id,
            version: file.version.clone(),
            unit: UnitKey::new(1, 1),
            intro_start_ms: Some(3_000),
            intro_end_ms: Some(5_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "verified".into(),
            locked: true,
            updated_at: 0,
        })
        .unwrap();
    f.state.probe = Arc::new(
        api::probe_manager::ProbeManager::new(f.state.store())
            .unwrap()
            .with_tracks_probe(Arc::new(DurationTracks)),
    );
    f.state = f.state.with_chapter_frame_runner(Arc::new(Frames));
    f.app = api::router(f.state.clone());
    f
}

fn last_image(file: domain::LedgerId, snapshot: &store::ChapterFrameTaskSnapshot) -> String {
    let target = snapshot.targets.last().unwrap();
    let artifact = target.published.as_ref().unwrap();
    format!(
        "/chapters/{file}/{}?generation={}&artifact={}-{}",
        target.index, snapshot.status.generation, artifact.attempt, artifact.id
    )
}

async fn probe_duration(f: &Fixture) {
    let row = f
        .store
        .file_coverage_by_id(f.files[0].file_id)
        .unwrap()
        .unwrap();
    assert!(f.state.probe.enqueue_force(api::probe_manager::ProbeUnit {
        kind: row.file.kind,
        row,
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: false,
        marker_refresh_id: None,
        job_id: None,
    }));
    assert!(f.state.probe.try_start_workers());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let job = f
                .store
                .latest_probe_job_for_scope(&format!("ledger:{}", f.files[0].file_id))
                .unwrap()
                .unwrap();
            if !job.is_active() {
                assert_eq!(job.status, "succeeded");
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("metadata job must finish without a marker publication");
}

#[test]
fn duration_only_metadata_publication_wakes_the_idle_frame_service() {
    let f = fixture();
    let file = &f.files[0];
    let marker = f
        .store
        .get_file_marker(file.file_id, UnitKey::new(1, 1))
        .unwrap()
        .unwrap();
    let service = f.state.chapter_frames();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        service.start();
        let old = super::service::wait_state(&f, store::ChapterFrameState::Completed).await;
        while service.pending_memory() != 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(service.dispatch_once().await.unwrap(), 0);
        // Let both workers finish their empty refill and await the next wakeup.
        tokio::time::sleep(Duration::from_millis(30)).await;
        let old_url = last_image(file.file_id, &old);
        assert_eq!(f.get(&old_url).await.status(), StatusCode::OK);
        probe_duration(&f).await;
        let row = f.store.file_coverage_by_id(file.file_id).unwrap().unwrap();
        assert_eq!(row.file.duration_ms, Some(20_000));
        assert_eq!(row.file.version, file.version);
        assert_eq!(
            f.store
                .get_file_marker(file.file_id, UnitKey::new(1, 1))
                .unwrap()
                .unwrap(),
            marker
        );
        assert_eq!(f.get(&old_url).await.status(), StatusCode::NOT_FOUND);
        let replacement = f
            .store
            .current_chapter_frame_task(file.file_id)
            .unwrap()
            .unwrap();
        assert_ne!(replacement.task.id, old.task.id);
        assert_eq!(replacement.task.targets.last(), Some(&(5_000, 20_000)));
        assert_eq!(
            f.store
                .chapter_frame_task_status(&old.task.id)
                .unwrap()
                .unwrap()
                .state,
            store::ChapterFrameState::Superseded
        );
        let completed = tokio::time::timeout(
            Duration::from_secs(2),
            super::service::wait_state(&f, store::ChapterFrameState::Completed),
        )
        .await;
        f.state.probe.shutdown_workers().await.unwrap();
        service.shutdown().await.unwrap();
        let completed = completed.unwrap_or_else(|error| {
            panic!(
                "duration publication did not dispatch replacement: {error}; current={:?}",
                f.store.current_chapter_frame_task(file.file_id).unwrap()
            )
        });
        let response = f.get(&last_image(file.file_id, &completed)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).is_ok());
    });
}
