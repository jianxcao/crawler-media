use std::sync::Arc;
use std::time::Duration;

use api::probe_manager::{ProbeManager, ProbeUnit};
use api::{ScrapeStoreExt, Store};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use marker::{AudioFingerprint, CommonSegment, FingerprintEngine};
use parking_lot::Mutex;

struct FixtureFingerprint;

impl FingerprintEngine for FixtureFingerprint {
    fn extract_at(&self, _: &std::path::Path, _: u32, _: u32) -> Result<AudioFingerprint, String> {
        Ok(vec![1, 2, 3, 4])
    }

    fn find_common_segment(&self, _: &[u32], _: &[u32], _: f32, _: f32) -> Option<CommonSegment> {
        None
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    store: Arc<Mutex<Store>>,
    manager: Arc<ProbeManager>,
    rows: Vec<LedgerRow>,
    jobs: Vec<String>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(Store::open(temp.path()).unwrap()));
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
        store.lock().insert_media(&media).unwrap();
        let root = temp.path().to_str().unwrap();
        let library = store
            .lock()
            .create_library(MediaKind::Tv, "TV", &[root], "everyone", true, &[])
            .unwrap();
        store
            .lock()
            .set_library_intro_settings(&library.id, true, true)
            .unwrap();
        let mut setting = store.lock().get_scrape_config().unwrap().setting;
        setting.fingerprint_duration_secs = Some(90);
        store.lock().save_scrape_config(&setting).unwrap();
        let manager = Arc::new(ProbeManager::with_fingerprint_engine(
            store.clone(),
            Arc::new(FixtureFingerprint),
        ));
        let mut rows = Vec::new();
        let mut jobs = Vec::new();
        for episode in 1..=33 {
            let row = LedgerRow {
                id: LedgerId::new(),
                media_id,
                path: temp
                    .path()
                    .join(format!("episode-{episode}.mkv"))
                    .display()
                    .to_string(),
                season: Some(1),
                episode: Some(episode),
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
            };
            std::fs::write(&row.path, b"fixture video").unwrap();
            let version =
                api::fingerprint_job::current_source_version(std::path::Path::new(&row.path));
            store.lock().insert_ledger(&row).unwrap();
            store
                .lock()
                .put_file_meta_versioned(
                    &row.id.to_string(),
                &library::Tracks {
                    video: Some(library::VideoTrack::default()),
                    ..Default::default()
                },
                    Some(&version),
                    Some(1_200_000),
                )
                .unwrap();
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
                    version,
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
                        .lock()
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
                self.store.lock().get_probe_job(id).unwrap().unwrap().status,
                "succeeded"
            );
        }
        for row in &self.rows[cancelled..] {
            let cache = self
                .store
                .lock()
                .get_fingerprint_cache(&row.id.to_string())
                .unwrap()
                .unwrap();
            assert!(!cache.intro.is_empty(), "{}", row.path);
            assert!(
                cache.outro.is_some_and(|outro| !outro.is_empty()),
                "{}",
                row.path
            );
            assert_eq!(cache.sample_duration_secs, 90);
            assert_eq!(cache.media_duration_ms, Some(1_200_000));
        }
        assert_eq!(self.manager.queued_ledger_count(), 0);
    }
}

#[tokio::test]
async fn fingerprint_overflow_refills_after_success_without_metadata_work() {
    Fixture::new().drain(0).await;
}

#[tokio::test]
async fn fingerprint_overflow_refills_after_every_initial_source_is_deleted() {
    let fixture = Fixture::new();
    for row in &fixture.rows[..16] {
        std::fs::remove_file(&row.path).unwrap();
    }
    fixture.drain(16).await;
    for id in &fixture.jobs[..16] {
        assert_eq!(
            fixture
                .store
                .lock()
                .get_probe_job(id)
                .unwrap()
                .unwrap()
                .status,
            "cancelled"
        );
    }
}
