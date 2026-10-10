use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use api::probe_manager::policy::{ProbeRequestOrigin, ProbeRequestResult};
use api::probe_manager::ProbeManager;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use marker::FingerprintEngine;
use parking_lot::Mutex;
use store::{FingerprintCacheEntry, Store};

pub struct FakeFingerprintEngine {
    pub intro_words: Vec<u32>,
    pub outro_words: Vec<u32>,
    pub outro_error: Mutex<Option<String>>,
    pub metadata_calls: Mutex<usize>,
    pub intro_calls: Mutex<usize>,
    pub outro_calls: Mutex<usize>,
}

impl FakeFingerprintEngine {
    pub fn new() -> Self {
        Self {
            intro_words: vec![1, 2, 3, 4],
            outro_words: vec![5, 6, 7, 8],
            outro_error: Mutex::new(None),
            metadata_calls: Mutex::new(0),
            intro_calls: Mutex::new(0),
            outro_calls: Mutex::new(0),
        }
    }
}

impl FingerprintEngine for FakeFingerprintEngine {
    fn extract_at(
        &self,
        _path: &Path,
        start_secs: u32,
        _duration_secs: u32,
    ) -> Result<marker::AudioFingerprint, String> {
        if start_secs == 0 {
            *self.intro_calls.lock() += 1;
            Ok(self.intro_words.clone())
        } else {
            *self.outro_calls.lock() += 1;
            if let Some(err) = self.outro_error.lock().clone() {
                Err(err)
            } else {
                Ok(self.outro_words.clone())
            }
        }
    }

    fn find_common_segment(
        &self,
        _first: &[u32],
        _second: &[u32],
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Option<marker::CommonSegment> {
        None
    }
}

pub struct ProbeScenario {
    pub tmp: tempfile::TempDir,
    pub store: Arc<Mutex<Store>>,
    pub manager: Arc<ProbeManager>,
    pub engine: Arc<FakeFingerprintEngine>,
    pub media_id: MediaId,
    pub ledgers: Vec<LedgerRow>,
}

impl ProbeScenario {
    pub fn cached_season() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
        let engine = Arc::new(FakeFingerprintEngine::new());
        let manager = Arc::new(ProbeManager::with_fingerprint_engine(
            store.clone(),
            engine.clone(),
        ));
        manager.try_start_workers();

        let media_id = MediaId::new();
        let root = tmp.path().join("tv_root");
        std::fs::create_dir_all(&root).unwrap();

        let library = store.lock().create_library(
            MediaKind::Tv,
            "TV Show Library",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        ).unwrap();
        store.lock().set_library_intro_settings(&library.id, true, true).unwrap();

        let media = Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Test Comedy".into(),
            year: Some(2026),
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.lock().insert_media(&media).unwrap();

        let mut ledgers = Vec::new();
        for ep in 1..=8 {
            let ep_path = root.join(format!("S01E0{ep}.strm"));
            std::fs::write(&ep_path, format!("http://127.0.0.1:8080/ep{ep}.mkv\n")).unwrap();
            let row = LedgerRow {
                id: LedgerId::new(),
                media_id,
                path: ep_path.to_str().unwrap().to_string(),
                season: Some(1),
                episode: Some(ep),
                resolution: Some("1080p".into()),
                codec: Some("H264".into()),
                hdr: None,
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                filter_score: None,
            };
            store.lock().insert_ledger(&row).unwrap();

            // Populate cached media info
            let source_version = api::fingerprint_job::current_source_version(Path::new(&row.path));
            let tracks = library::Tracks {
                video: Some(library::VideoTrack {
                    codec: Some("h264".into()),
                    width: Some(1920),
                    height: Some(1080),
                    duration_secs: Some(2400.0),
                    ..library::VideoTrack::default()
                }),
                ..library::Tracks::default()
            };
            store.lock().put_file_meta_versioned(
                &row.id.to_string(),
                &tracks,
                Some(&source_version),
                Some(2400_000), // 40 minutes
            ).unwrap();

            // Populate cached fingerprint
            let cache_key = api::fingerprint_job::fingerprint_cache_key(&source_version, 180, Some(2400_000));
            store.lock().put_fingerprint_cache(
                &row.id.to_string(),
                &FingerprintCacheEntry {
                    cache_key,
                    algorithm_version: marker::FINGERPRINT_ALGORITHM_VERSION,
                    sample_duration_secs: 180,
                    media_duration_ms: Some(2400_000),
                    intro: vec![1, 2, 3],
                    outro: Some(vec![4, 5, 6]),
                },
            ).unwrap();

            ledgers.push(row);
        }

        ProbeScenario {
            tmp,
            store,
            manager,
            engine,
            media_id,
            ledgers,
        }
    }

    pub fn missing_outro(&mut self, episode: u32) {
        let row = self.ledgers.iter().find(|l| l.episode == Some(episode)).unwrap();
        let mut cache = self.store.lock().get_fingerprint_cache(&row.id.to_string()).unwrap().unwrap();
        cache.outro = None;
        self.store.lock().put_fingerprint_cache(&row.id.to_string(), &cache).unwrap();
    }

    pub fn fail_next_outro(&self, _episode: u32, error: &str) {
        *self.engine.outro_error.lock() = Some(error.to_string());
    }

    pub fn request(&self, episode: u32, origin: ProbeRequestOrigin) -> ProbeRequestResult {
        let row = self.ledgers.iter().find(|l| l.episode == Some(episode)).unwrap();
        self.manager.request_probe(row, origin).unwrap()
    }

    pub async fn drain(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let active = self.store.lock().recover_probe_jobs().unwrap();
            if active.is_empty() || std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub fn metadata_reads(&self, _episode: u32) -> usize {
        *self.engine.metadata_calls.lock()
    }

    pub fn intro_reads(&self, _episode: u32) -> usize {
        *self.engine.intro_calls.lock()
    }

    pub fn outro_reads(&self, _episode: u32) -> usize {
        *self.engine.outro_calls.lock()
    }

    pub fn fingerprint_job_status(&self, episode: u32) -> String {
        let row = self.ledgers.iter().find(|l| l.episode == Some(episode)).unwrap();
        let store = self.store.lock();
        store
            .latest_probe_job_for_scope(&format!("ledger:{}", row.id))
            .unwrap()
            .map(|job| job.status)
            .unwrap_or_else(|| "none".to_string())
    }
}
