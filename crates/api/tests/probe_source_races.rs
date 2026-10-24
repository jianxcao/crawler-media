#[path = "probe_retry_support.rs"]
mod support;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use api::probe_manager::{ProbeManager, ProbeUnit};
use domain::{FileCoverage, FileVersion, MediaKind};
use marker::{AudioFingerprint, CommonSegment, FingerprintEngine};
use parking_lot::Mutex;
use store::Store;
use support::ProbeScenario;

struct ReplaceSource {
    store: Arc<Mutex<Store>>,
    row: FileCoverage,
    during_capture: bool,
    publish_replacement: bool,
    replaced: AtomicBool,
    start_secs: AtomicU32,
}

impl ReplaceSource {
    fn replace(&self) {
        if self.replaced.swap(true, Ordering::SeqCst) {
            return;
        }
        std::fs::write(
            &self.row.file.path,
            "https://example.invalid/new-source.mkv\n",
        )
        .unwrap();
        if !self.publish_replacement {
            return;
        }
        let mut file = self.row.file.clone();
        file.version = FileVersion {
            revision: file.version.revision + 1,
            source_version: library::observe_source(&file.path).unwrap().source_version,
        };
        let store = self.store.lock();
        store
            .publish_files(
                &store::library_write::WriteScope {
                    library_id: file.library_id,
                    config_revision: store
                        .library_config_revision(&file.library_id.to_string())
                        .unwrap(),
                },
                &[store::library_write::FilePublication {
                    file: &file,
                    units: &self.row.units,
                    expected_version: Some(&self.row.file.version),
                    nfo_observation: None,
                    probe_required: false,
                }],
                2_000,
            )
            .unwrap();
    }
}

impl FingerprintEngine for ReplaceSource {
    fn extract_at(&self, path: &Path, _: u32, _: u32) -> Result<AudioFingerprint, String> {
        if self.during_capture && path == self.row.file.path {
            self.replace();
        }
        Ok(vec![8, 9, 10, 11])
    }

    fn find_common_segment(&self, _: &[u32], _: &[u32], _: f32, _: f32) -> Option<CommonSegment> {
        if !self.during_capture {
            self.replace();
        }
        let start = self.start_secs.load(Ordering::SeqCst) as f32;
        Some(CommonSegment {
            start1_sec: start,
            end1_sec: start + 50.0,
            start2_sec: start,
            end2_sec: start + 50.0,
            duration_sec: 50.0,
            score: 1.0,
        })
    }
}

async fn scenario(
    during_capture: bool,
    publish_replacement: bool,
) -> (ProbeScenario, Arc<ReplaceSource>) {
    let mut scenario = ProbeScenario::cached_season();
    scenario.manager.shutdown_workers().await.unwrap();
    let engine = Arc::new(ReplaceSource {
        store: scenario.store.clone(),
        row: scenario.ledgers[0].clone(),
        during_capture,
        publish_replacement,
        replaced: AtomicBool::new(false),
        start_secs: AtomicU32::new(10),
    });
    scenario.manager = Arc::new(ProbeManager::with_fingerprint_engine(
        scenario.store.clone(),
        engine.clone(),
    ));
    scenario.manager.try_start_workers();
    (scenario, engine)
}

async fn refresh(scenario: &ProbeScenario) -> store::ProbeJob {
    refresh_rows(scenario, &scenario.ledgers).await
}

async fn refresh_rows(scenario: &ProbeScenario, rows: &[FileCoverage]) -> store::ProbeJob {
    let units = rows
        .iter()
        .map(|row| ProbeUnit {
            row: row.clone(),
            kind: MediaKind::Tv,
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: true,
            marker_refresh_id: None,
            job_id: None,
        })
        .collect();
    assert_eq!(scenario.manager.enqueue_marker_refresh(units), rows.len());
    let job = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let job = scenario
                .store
                .lock()
                .latest_marker_refresh_for_season(&scenario.media_id.to_string(), 1)
                .unwrap()
                .unwrap();
            if !job.is_active() {
                return job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("source race must reach a persisted terminal state");
    scenario.manager.shutdown_workers().await.unwrap();
    job
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disk_change_during_comparison_cannot_publish_old_markers() {
    let (scenario, engine) = scenario(false, false).await;
    let job = refresh(&scenario).await;
    assert!(engine.replaced.load(Ordering::SeqCst));
    let store = scenario.store.lock();
    assert!(
        store
            .get_media_marker(scenario.media_id, Some(1), Some(1))
            .unwrap()
            .is_none(),
        "marker computed from the previous STRM target must not publish"
    );
    assert!(
        store
            .get_cached_chapters(&engine.row.file.file_id.to_string())
            .unwrap()
            .is_none()
    );
    assert_ne!(job.status, "succeeded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ledger_change_during_comparison_cannot_publish_old_markers() {
    let (scenario, engine) = scenario(false, true).await;
    let job = refresh(&scenario).await;
    assert!(engine.replaced.load(Ordering::SeqCst));
    let store = scenario.store.lock();
    assert!(
        store
            .get_media_marker(scenario.media_id, Some(1), Some(1))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_cached_chapters(&engine.row.file.file_id.to_string())
            .unwrap()
            .is_none()
    );
    assert_ne!(job.status, "succeeded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_refresh_checks_comparison_inputs_outside_its_lease() {
    let (scenario, engine) = scenario(false, false).await;
    let job = refresh_rows(&scenario, &scenario.ledgers[1..3]).await;
    assert!(engine.replaced.load(Ordering::SeqCst));
    let store = scenario.store.lock();
    for episode in [2, 3] {
        assert!(
            store
                .get_media_marker(scenario.media_id, Some(1), Some(episode))
                .unwrap()
                .is_none(),
            "the changed unleased sample participated in this comparison"
        );
    }
    assert_ne!(job.status, "succeeded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disk_change_during_capture_cannot_overwrite_fingerprint_cache() {
    let (scenario, engine) = scenario(true, false).await;
    let file_id = engine.row.file.file_id.to_string();
    let before = scenario
        .store
        .lock()
        .get_fingerprint_cache(&file_id)
        .unwrap()
        .unwrap();
    let job = refresh(&scenario).await;
    assert!(engine.replaced.load(Ordering::SeqCst));
    let after = scenario
        .store
        .lock()
        .get_fingerprint_cache(&file_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        after.intro, before.intro,
        "unscanned disk replacement must reject the old capture"
    );
    assert_ne!(job.status, "succeeded");
}

struct ReplacingTracks;

impl api::probe_manager::TracksProbe for ReplacingTracks {
    fn probe(
        &self,
        path: &Path,
        _: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        std::fs::write(path, "https://example.invalid/replaced-during-tracks.mkv\n")?;
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
            Some(99_000),
        ))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disk_change_during_tracks_cannot_publish_details_or_nfo() {
    let mut scenario = ProbeScenario::cached_season();
    scenario.manager.shutdown_workers().await.unwrap();
    scenario.manager = Arc::new(
        ProbeManager::with_fingerprint_engine(scenario.store.clone(), scenario.engine.clone())
            .with_tracks_probe(Arc::new(ReplacingTracks)),
    );
    let row = scenario.ledgers[0].clone();
    let nfo_path = row.file.path.with_extension("nfo");
    let before_nfo = "<episodedetails><title>Original</title></episodedetails>";
    std::fs::write(&nfo_path, before_nfo).unwrap();
    let file_id = row.file.file_id.to_string();
    assert!(scenario.manager.enqueue_force(ProbeUnit {
        row,
        kind: MediaKind::Tv,
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: false,
        marker_refresh_id: None,
        job_id: None,
    }));
    scenario.manager.try_start_workers();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let job = scenario
                .store
                .lock()
                .latest_probe_job_for_scope(&format!("ledger:{file_id}"))
                .unwrap()
                .unwrap();
            if !job.is_active() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("metadata race must terminate");
    scenario.manager.shutdown_workers().await.unwrap();
    let details = scenario
        .store
        .lock()
        .get_file_meta(&file_id)
        .unwrap()
        .unwrap();
    assert_eq!(details.video.unwrap().width, Some(1920));
    assert_eq!(
        std::fs::read_to_string(nfo_path).unwrap(),
        before_nfo,
        "a stale stream probe must not modify NFO sidecars"
    );
}

async fn probe_once(scenario: &ProbeScenario, row: FileCoverage) {
    let id = row.file.file_id.to_string();
    assert!(scenario.manager.enqueue(ProbeUnit {
        row,
        kind: MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: true,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    }));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let terminal = scenario
                .store
                .lock()
                .latest_probe_job_for_scope(&format!("ledger:{id}"))
                .unwrap()
                .is_some_and(|job| !job.is_active());
            if terminal && !scenario.manager.is_queued(&id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("probe must finish and release its queue identity");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacement_with_the_same_sample_lengths_must_recompare() {
    let (scenario, engine) = scenario(true, false).await;
    probe_once(&scenario, scenario.ledgers[1].clone()).await;
    assert!(
        scenario
            .store
            .lock()
            .get_media_marker(scenario.media_id, Some(1), Some(1))
            .unwrap()
            .is_some()
    );
    let mut before = scenario
        .store
        .lock()
        .get_fingerprint_cache(&engine.row.file.file_id.to_string())
        .unwrap()
        .unwrap();
    ReplaceSource {
        store: scenario.store.clone(),
        row: engine.row.clone(),
        during_capture: false,
        publish_replacement: true,
        replaced: AtomicBool::new(false),
        start_secs: AtomicU32::new(10),
    }
    .replace();
    let source_version = library::observe_source(&engine.row.file.path)
        .unwrap()
        .source_version;
    scenario
        .store
        .lock()
        .put_file_meta_versioned(
            &engine.row.file.file_id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack::default()),
                ..Default::default()
            },
            Some(&source_version),
            Some(2_400_000),
        )
        .unwrap();
    before.cache_key =
        api::fingerprint_job::fingerprint_cache_key(&source_version, 180, Some(2_400_000));
    before.intro = vec![21, 22, 23];
    before.outro = Some(vec![24, 25, 26]);
    scenario
        .store
        .lock()
        .put_fingerprint_cache(&engine.row.file.file_id.to_string(), &before)
        .unwrap();
    engine.start_secs.store(30, Ordering::SeqCst);
    probe_once(&scenario, scenario.ledgers[1].clone()).await;
    scenario.manager.shutdown_workers().await.unwrap();
    assert_eq!(
        scenario
            .store
            .lock()
            .get_media_marker(scenario.media_id, Some(1), Some(1))
            .unwrap()
            .unwrap()
            .intro_start_ms,
        Some(30_000),
        "the replacement has fresh samples and needs a new marker comparison even at equal sample lengths"
    );
}
