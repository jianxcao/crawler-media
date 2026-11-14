use std::sync::Arc;
use std::time::Duration;

use api::probe_manager::clock::ProbeClock;
use api::probe_manager::{ProbeManager, ProbeUnit};
use api::{ScrapeStoreExt, Store};
use domain::{
    Confidence, FileCoverage, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use marker::{AudioFingerprint, CommonSegment, FingerprintEngine};
use store::ScannedLedger;

fn source_facts() -> marker::CaptureSourceFacts {
    marker::CaptureSourceFacts {
        audio_stream_index: 0,
        format_start_time_us: Some(0),
        stream_start_pts: Some(0),
        stream_time_base: Some("1/16000".into()),
        codec_name: None,
        native_sample_rate: None,
        audio_channels: None,
        provenance: "fixture_header".into(),
    }
}

struct FixtureFingerprint;

impl FingerprintEngine for FixtureFingerprint {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _control: &marker::WorkControl,
    ) -> Result<AudioFingerprint, String> {
        Ok(vec![1, 2, 3, 4])
    }

    fn find_common_segment(&self, _: &[u32], _: &[u32], _: f32, _: f32) -> Option<CommonSegment> {
        None
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    store: Arc<Store>,
    manager: Arc<ProbeManager>,
    rows: Vec<FileCoverage>,
    jobs: Vec<String>,
}

/// Insert one episode file through the scan commit (`insert_scanned_files`), then queue
/// direct fingerprint work for it with fixture metadata so no ffprobe or network IO runs.
fn insert_episode(
    store: &Store,
    media_id: MediaId,
    path: &std::path::Path,
    episode: u32,
) -> FileCoverage {
    std::fs::write(path, b"fixture video").unwrap();
    let version = api::fingerprint_job::current_source_version(path);
    let file_id = LedgerId::new();
    store
        .insert_scanned_files(
            &[ScannedLedger {
                file_id,
                media_id,
                path: path.display().to_string(),
                units: vec![UnitKey::new(1, i32::try_from(episode).unwrap())],
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
                source_version: version.clone(),
                source_digest: None,
                size_bytes: None,
                created_ns: None,
                modified_ns: None,
                artwork: Default::default(),
                observations: Vec::new(),
            }],
            1_000,
        )
        .unwrap();
    store
        .put_file_meta_versioned(
            &file_id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack::default()),
                audio: vec![library::AudioTrack {
                    stream_index: Some(0),
                    capture_source: Some(source_facts()),
                    is_default: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            Some(&version),
            Some(1_200_000),
        )
        .unwrap();
    store
        .file_coverage_by_id(file_id)
        .unwrap()
        .expect("scanned episode row")
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(temp.path()).unwrap());
        let media_id = MediaId::new();
        let media = Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Fingerprint backlog".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.as_ref().insert_media(&media).unwrap();
        let root = temp.path().to_str().unwrap();
        let library = store
            .as_ref()
            .create_library(MediaKind::Tv, "TV", &[root], "everyone", true, &[])
            .unwrap();
        store
            .as_ref()
            .set_library_intro_settings(&library.id, true, true)
            .unwrap();
        let mut setting = store.as_ref().get_scrape_config().unwrap().setting;
        setting.fingerprint_duration_secs = Some(90);
        store.as_ref().save_scrape_config(&setting).unwrap();
        let manager = Arc::new(
            ProbeManager::with_engines(
                store.clone(),
                Arc::new(FixtureFingerprint),
                Arc::new(FixtureFingerprint),
            )
            .unwrap(),
        );
        let mut rows = Vec::new();
        let mut jobs = Vec::new();
        for episode in 1..=33 {
            let path = temp.path().join(format!("episode-{episode}.mkv"));
            let row = insert_episode(store.as_ref(), media_id, &path, episode);
            let job = manager
                .enqueue_direct_fingerprint(
                    ProbeUnit {
                        row: row.clone(),
                        kind: MediaKind::Tv,
                        force_fingerprint: true,
                        reuse_fingerprint_cache: true,
                        overwrite_markers: false,
                        reuse_media_info_cache: true,
                        marker_refresh_id: None,
                        job_id: None,
                    },
                    Some(1_200_000),
                    row.file.version.source_version.clone(),
                    90,
                )
                .unwrap();
            rows.push(row);
            jobs.push(job);
        }
        Self {
            _temp: temp,
            store,
            manager,
            rows,
            jobs,
        }
    }

    async fn drain(&self, cancelled: usize) {
        assert_eq!(self.manager.queued_ledger_count(), 16);
        assert!(self.manager.try_start_workers());
        let drained = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self.jobs.iter().all(|id| {
                    !self
                        .store
                        .as_ref()
                        .get_probe_job(id)
                        .unwrap()
                        .unwrap()
                        .is_active()
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        self.manager.shutdown_workers().await.unwrap();
        assert!(
            drained.is_ok(),
            "fingerprint overflow must drain without metadata work or a restart"
        );
        for id in &self.jobs[cancelled..] {
            assert_eq!(
                self.store
                    .as_ref()
                    .get_probe_job(id)
                    .unwrap()
                    .unwrap()
                    .status,
                "succeeded"
            );
        }
        for row in &self.rows[cancelled..] {
            let cache = self
                .store
                .as_ref()
                .get_fingerprint_cache(&row.file.file_id.to_string())
                .unwrap()
                .unwrap();
            assert!(!cache.intro.words.is_empty(), "{}", row.file.path.display());
            assert!(
                cache.outro.is_some_and(|outro| !outro.words.is_empty()),
                "{}",
                row.file.path.display()
            );
            assert_eq!(cache.sample_duration_secs, 90);
            assert_eq!(cache.media_duration_ms, Some(1_200_000));
        }
        assert_eq!(self.manager.queued_ledger_count(), 0);
        for id in &self.jobs[..cancelled] {
            let job = self.store.get_probe_job(id).unwrap().unwrap();
            assert_eq!(job.status, "cancelled");
            assert!(
                job.started_at_ms.is_none(),
                "deleted input never starts work"
            );
            assert!(
                self.manager.timing_snapshot(id).is_none(),
                "uncaptured cancelled input has no measured work"
            );
        }
        let computed = self.jobs[cancelled..]
            .iter()
            .map(|id| {
                self.manager
                    .timing_snapshot(id)
                    .unwrap_or_else(|| {
                        panic!(
                            "missing measured timing for {:?}",
                            self.store.get_probe_job(id).unwrap().unwrap()
                        )
                    })
                    .raw_pairs_computed
                    .expect("every surviving job records its measured Analysis contribution")
            })
            .sum::<u64>();
        let files = (self.rows.len() - cancelled) as u64;
        assert_eq!(
            computed,
            files * (files - 1),
            "each kind visits one direction per pair when the raw result is NoMatch; shared work is attributed once"
        );
    }
}

#[tokio::test]
async fn fingerprint_overflow_refills_after_success_without_metadata_work() {
    Fixture::new().drain(0).await;
}

fn add_fairness_seasons(fixture: &mut Fixture) {
    for number in 0..17 {
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: format!("season-{number}"),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        fixture.store.insert_media(&media).unwrap();
        for episode in 1..=if number == 16 { 24 } else { 1 } {
            let path = fixture
                ._temp
                .path()
                .join(format!("media-{number}-episode-{episode}.mkv"));
            let row = insert_episode(&fixture.store, media.id, &path, episode);
            let job = fixture
                .manager
                .enqueue_direct_fingerprint(
                    ProbeUnit {
                        row: row.clone(),
                        kind: MediaKind::Tv,
                        force_fingerprint: true,
                        reuse_fingerprint_cache: true,
                        overwrite_markers: false,
                        reuse_media_info_cache: true,
                        marker_refresh_id: None,
                        job_id: None,
                    },
                    Some(1_200_000),
                    row.file.version.source_version.clone(),
                    90,
                )
                .unwrap();
            fixture.jobs.push(job);
            fixture.rows.push(row);
        }
    }
}

#[tokio::test]
async fn analysis_horizon_serves_other_seasons_while_hot_season_keeps_arriving() {
    let mut fixture = Fixture::new();
    let clock = Arc::new(api::probe_manager::clock::FakeClock::new(0));
    fixture.manager.set_clock(clock.clone());
    add_fairness_seasons(&mut fixture);
    fixture.manager.try_start_workers();
    let cold = fixture.rows[fixture.rows.len() - 24..].to_vec();
    let mut arrivals = 0;
    let mut observed_round = None;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let cold_pending = cold.iter().any(|row| {
                fixture
                    .store
                    .active_probe_unit_for_ledger(&row.file.file_id.to_string())
                    .unwrap()
                    .is_some()
            });
            let round = fixture
                .manager
                .comparison_runs
                .load(std::sync::atomic::Ordering::Relaxed);
            // Sustain arrivals across real analysis turns without making the
            // injected backlog depend on unrelated tests' wall-clock CPU cost.
            if cold_pending && observed_round != Some(round) {
                observed_round = Some(round);
                let path = fixture
                    ._temp
                    .path()
                    .join(format!("hot-arrival-{arrivals}.mkv"));
                let row = insert_episode(
                    &fixture.store,
                    fixture.rows[0].file.media_id,
                    &path,
                    100 + arrivals,
                );
                let job = fixture
                    .manager
                    .enqueue_direct_fingerprint(
                        ProbeUnit {
                            row: row.clone(),
                            kind: MediaKind::Tv,
                            force_fingerprint: true,
                            reuse_fingerprint_cache: true,
                            overwrite_markers: false,
                            reuse_media_info_cache: true,
                            marker_refresh_id: None,
                            job_id: None,
                        },
                        Some(1_200_000),
                        row.file.version.source_version.clone(),
                        90,
                    )
                    .unwrap();
                fixture.jobs.push(job);
                fixture.rows.push(row);
                arrivals += 1;
            }
            clock.set(clock.now_ms() + 2_000);
            fixture.manager.wake_dispatch();
            if !cold_pending {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cold season must finish while hot arrivals continue");
    assert!(
        fixture
            .rows
            .iter()
            .filter(|row| row.file.media_id == fixture.rows[0].file.media_id)
            .any(|row| fixture
                .store
                .active_probe_unit_for_ledger(&row.file.file_id.to_string())
                .unwrap()
                .is_some()),
        "cold work is served before the hot season drains"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.rows.iter().any(|row| {
            fixture
                .store
                .active_probe_unit_for_ledger(&row.file.file_id.to_string())
                .unwrap()
                .is_some()
        }) {
            clock.set(clock.now_ms() + 2_000);
            fixture.manager.wake_dispatch();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("finite work drains after the producer stops");
    fixture.manager.shutdown_workers().await.unwrap();
    assert!(
        arrivals > 1,
        "fixture must overlap several hot arrivals with cold work"
    );
    assert!(
        fixture
            .jobs
            .iter()
            .all(|id| fixture.store.get_probe_job(id).unwrap().unwrap().status == "succeeded")
    );
}

#[tokio::test]
async fn capture_handoff_is_durable_before_quiet_analysis_and_reopens_without_recapture() {
    let fixture = Fixture::new();
    let clock = Arc::new(api::probe_manager::clock::FakeClock::new(0));
    fixture.manager.set_clock(clock.clone());
    assert!(fixture.manager.try_start_workers());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let ready = fixture.jobs.iter().all(|id| {
                fixture
                    .store
                    .probe_job_units(id)
                    .unwrap()
                    .iter()
                    .all(|unit| unit.stage == store::ProbeDispatchStage::Analysis)
            });
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("all 33 captures must leave the 16-slot window before quiet time advances");
    assert!(fixture.jobs.iter().all(|id| {
        fixture
            .store
            .get_probe_job(id)
            .unwrap()
            .unwrap()
            .is_active()
    }));
    assert_eq!(fixture.manager.queued_ledger_count(), 0);
    fixture.manager.shutdown_workers().await.unwrap();
    let mut changed = fixture.store.get_scrape_config().unwrap().setting;
    changed.fingerprint_sampling_mode = Some("adaptive".into());
    changed.fingerprint_duration_secs = Some(17);
    fixture.store.save_scrape_config(&changed).unwrap();
    let pending = fixture
        .store
        .probe_analysis_participants(fixture.rows[0].file.media_id, 1, None, 16)
        .unwrap();
    assert!(pending.iter().all(|p| {
        p.context().sampling_mode == "full_window"
            && serde_json::from_str::<marker::SamplingPolicy>(&p.context().analysis_policy_json)
                .unwrap()
                .full_window_duration_secs
                == 90
    }));
    let resumed = Arc::new(
        ProbeManager::with_engines(
            fixture.store.clone(),
            Arc::new(FixtureFingerprint),
            Arc::new(RefuseRecapture),
        )
        .unwrap(),
    );
    resumed.set_clock(clock.clone());
    let readers: Vec<_> = (0..8)
        .map(|_| {
            fixture
                .store
                .reader_pool(store::Db::Library)
                .checkout(Duration::ZERO)
                .unwrap()
        })
        .collect();
    assert!(resumed.try_start_workers());
    clock.set(2_000);
    resumed.wake_dispatch();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        resumed
            .comparison_runs
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    drop(readers);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if resumed
                .comparison_runs
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                clock.set(clock.now_ms() + 2_000);
            }
            resumed.wake_dispatch();
            if fixture.jobs.iter().all(|id| {
                !fixture
                    .store
                    .get_probe_job(id)
                    .unwrap()
                    .unwrap()
                    .is_active()
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("recovered analysis completes");
    resumed.shutdown_workers().await.unwrap();
    assert!(
        fixture
            .jobs
            .iter()
            .all(|id| fixture.store.get_probe_job(id).unwrap().unwrap().status == "succeeded")
    );
}

struct RefuseRecapture;
impl marker::FingerprintCaptureEngine for RefuseRecapture {
    fn capture_window(
        &self,
        _: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        panic!("Analysis recovery must reuse durable capture")
    }
}

#[tokio::test]
async fn fingerprint_overflow_refills_after_every_initial_source_is_deleted() {
    let fixture = Fixture::new();
    for row in &fixture.rows[..16] {
        std::fs::remove_file(&row.file.path).unwrap();
    }
    fixture.drain(16).await;
    for id in &fixture.jobs[..16] {
        assert_eq!(
            fixture
                .store
                .as_ref()
                .get_probe_job(id)
                .unwrap()
                .unwrap()
                .status,
            "cancelled"
        );
    }
}

impl marker::FingerprintCaptureEngine for FixtureFingerprint {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        let words = marker::FingerprintEngine::extract_at(
            self,
            &request.path,
            (request.window.start_ms / 1000) as u32,
            (request.window.duration_ms() / 1000) as u32,
            &marker::WorkControl::default(),
        )
        .map_err(|message| marker::CaptureFailure {
            kind: marker::CaptureFailureKind::Decode,
            message,
            metrics: marker::CaptureMetrics::default(),
        })?;
        Ok(marker::CapturedFingerprint {
            window: request.window.clone(),
            words,
            pcm_duration_ms: Some(request.window.duration_ms()),
            metrics: marker::CaptureMetrics {
                time_mapping_version: 3,
                audio_stream_index: Some(0),
                source_facts: Some(source_facts()),
                pcm_timeline: marker::PcmTimeline::Contiguous,
                contiguous_pcm_start_ms: Some(request.window.start_ms),
                ..Default::default()
            },
        })
    }
}
