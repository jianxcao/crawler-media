use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use api::probe_manager::ProbeManager;
use api::probe_manager::clock::ProbeClock;
use api::probe_manager::policy::{ProbeRequestOrigin, ProbeRequestResult};
use domain::{Confidence, FileCoverage, LedgerId, Media, MediaId, MediaKind, QualitySource};
use marker::FingerprintEngine;
use parking_lot::Mutex;
use store::test_support::FixtureFile;
use store::{FingerprintCacheEntry, Store};

pub struct FakeFingerprintEngine {
    pub intro_words: Vec<u32>,
    pub outro_words: Vec<u32>,
    pub outro_error: Mutex<Option<String>>,
    pub metadata_calls: Mutex<HashMap<u32, usize>>,
    pub intro_calls: Mutex<HashMap<u32, usize>>,
    pub outro_calls: Mutex<HashMap<u32, usize>>,
}

impl FakeFingerprintEngine {
    pub fn new() -> Self {
        Self {
            intro_words: vec![1, 2, 3, 4],
            outro_words: vec![5, 6, 7, 8],
            outro_error: Mutex::new(None),
            metadata_calls: Mutex::new(HashMap::new()),
            intro_calls: Mutex::new(HashMap::new()),
            outro_calls: Mutex::new(HashMap::new()),
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
        let episode = episode_number(_path);
        if start_secs == 0 {
            *self.intro_calls.lock().entry(episode).or_insert(0) += 1;
            Ok(self.intro_words.clone())
        } else {
            *self.outro_calls.lock().entry(episode).or_insert(0) += 1;
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

fn episode_number(path: &Path) -> u32 {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| {
            name.trim_start_matches("S01E0")
                .trim_end_matches(".strm")
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

/// Publish one fixture row with the content-derived source version, exactly as a scan
/// does. The plain publication path stamps its own timestamp version, which would make
/// the probe manager see every cached metadata/fingerprint row as stale.
fn publish_row(store: &Store, spec: &FixtureFile, source_version: &str) -> FileCoverage {
    let path = Path::new(&spec.path);
    let media = store
        .get_media(spec.media_id)
        .unwrap()
        .expect("fixture media");
    let library = store
        .library_for_path_strict(path, media.kind)
        .unwrap()
        .expect("fixture path must belong to a Library");
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .filter(|root| root.library_id.as_str() == library.id.as_str())
        .filter(|root| path.starts_with(&root.path))
        .max_by_key(|root| root.path.components().count())
        .expect("fixture path must sit inside a Library root");
    let file = domain::LibraryFile {
        file_id: spec.file_id.unwrap(),
        library_id: library.id.parse().unwrap(),
        root_id: root.id,
        media_id: spec.media_id,
        kind: media.kind,
        path: path.to_path_buf(),
        version: domain::FileVersion {
            revision: 1,
            source_version: source_version.into(),
        },
        presence: domain::FilePresence::Present,
        publication: domain::FilePublication::Published,
        size_bytes: None,
        stat_known: false,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: spec.resolution.clone(),
        codec: spec.codec.clone(),
        hdr: spec.hdr.clone(),
        quality_source: spec.quality_source,
        confidence: spec.confidence,
        filter_score: spec.filter_score,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    let units = vec![domain::UnitKey::new(
        spec.season.unwrap() as i32,
        spec.episode.unwrap() as i32,
    )];
    let publication = store::library_write::FilePublication {
        file: &file,
        units: &units,
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    };
    store
        .publish_files(
            &store::library_write::WriteScope {
                library_id: file.library_id,
                config_revision: 0,
            },
            &[publication],
            1000,
        )
        .unwrap();
    FileCoverage {
        path_key: String::new(),
        file,
        units,
    }
}

pub struct ProbeScenario {
    pub tmp: tempfile::TempDir,
    pub store: Arc<Mutex<Store>>,
    pub manager: Arc<ProbeManager>,
    pub engine: Arc<FakeFingerprintEngine>,
    pub clock: Arc<api::probe_manager::clock::FakeClock>,
    pub media_id: MediaId,
    pub ledgers: Vec<FileCoverage>,
}

impl ProbeScenario {
    pub fn cached_season() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
        let engine = Arc::new(FakeFingerprintEngine::new());
        let clock = Arc::new(api::probe_manager::clock::FakeClock::new(1_000_000));
        let manager = Arc::new(ProbeManager::with_fingerprint_engine(
            store.clone(),
            engine.clone(),
        ));
        manager.set_clock(clock.clone());
        manager.try_start_workers();

        let media_id = MediaId::new();
        let root = tmp.path().join("tv_root");
        std::fs::create_dir_all(&root).unwrap();

        let library = store
            .lock()
            .create_library(
                MediaKind::Tv,
                "TV Show Library",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store
            .lock()
            .set_library_intro_settings(&library.id, true, true)
            .unwrap();

        let media = Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Test Comedy".into(),
            year: Some(2026),
            original_title: None,
            aliases: Vec::new(),
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
            let file_id = LedgerId::new();
            let spec = FixtureFile {
                file_id: Some(file_id),
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
                ..Default::default()
            };
            let source_version =
                api::fingerprint_job::current_source_version(Path::new(&spec.path));
            let coverage = publish_row(&store.lock(), &spec, &source_version);
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
            store
                .lock()
                .put_file_meta_versioned(
                    &file_id.to_string(),
                    &tracks,
                    Some(&source_version),
                    Some(2400_000), // 40 minutes
                )
                .unwrap();

            // Populate cached fingerprint
            let cache_key =
                api::fingerprint_job::fingerprint_cache_key(&source_version, 180, Some(2400_000));
            store
                .lock()
                .put_fingerprint_cache(
                    &file_id.to_string(),
                    &FingerprintCacheEntry {
                        cache_key,
                        algorithm_version: marker::FINGERPRINT_ALGORITHM_VERSION,
                        sample_duration_secs: 180,
                        media_duration_ms: Some(2400_000),
                        intro: vec![1, 2, 3],
                        outro: Some(vec![4, 5, 6]),
                    },
                )
                .unwrap();

            ledgers.push(coverage);
        }

        ProbeScenario {
            tmp,
            store,
            manager,
            engine,
            clock,
            media_id,
            ledgers,
        }
    }

    pub fn set_time_ms(&self, now_ms: i64) {
        self.clock.set(now_ms);
    }

    pub fn dispatch_due(&self) -> usize {
        self.manager.dispatch_due(self.clock.now_ms(), 32).unwrap()
    }

    pub fn reopen(&mut self) {
        drop(std::mem::replace(
            &mut self.manager,
            Arc::new(ProbeManager::with_fingerprint_engine(
                self.store.clone(),
                self.engine.clone(),
            )),
        ));
        self.manager.set_clock(self.clock.clone());
        self.manager.try_start_workers();
    }

    /// The published coverage of one season-1 episode of the fixture library.
    fn ledger_for_episode(&self, episode: u32) -> &FileCoverage {
        self.ledgers
            .iter()
            .find(|ledger| {
                ledger
                    .units
                    .iter()
                    .any(|unit| unit.episode_coordinates() == Some((1, episode)))
            })
            .unwrap()
    }

    pub fn missing_outro(&mut self, episode: u32) {
        let row = self.ledger_for_episode(episode);
        let file_id = row.file.file_id.to_string();
        let mut cache = self
            .store
            .lock()
            .get_fingerprint_cache(&file_id)
            .unwrap()
            .unwrap();
        cache.outro = None;
        self.store
            .lock()
            .put_fingerprint_cache(&file_id, &cache)
            .unwrap();
    }

    pub fn fail_next_outro(&self, _episode: u32, error: &str) {
        *self.engine.outro_error.lock() = Some(error.to_string());
    }

    pub fn request(&self, episode: u32, origin: ProbeRequestOrigin) -> ProbeRequestResult {
        let row = self.ledger_for_episode(episode);
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

    pub fn metadata_reads(&self, episode: u32) -> usize {
        self.engine
            .metadata_calls
            .lock()
            .get(&episode)
            .copied()
            .unwrap_or(0)
    }

    pub fn intro_reads(&self, episode: u32) -> usize {
        self.engine
            .intro_calls
            .lock()
            .get(&episode)
            .copied()
            .unwrap_or(0)
    }

    pub fn outro_reads(&self, episode: u32) -> usize {
        self.engine
            .outro_calls
            .lock()
            .get(&episode)
            .copied()
            .unwrap_or(0)
    }

    pub fn set_intro_settings(&self, detect_intros: bool, enable_fingerprint: bool) {
        let row = &self.ledgers[0];
        let store = self.store.lock();
        let library = store
            .library_for_path(row.file.path.as_path(), MediaKind::Tv)
            .unwrap()
            .unwrap();
        store
            .set_library_intro_settings(&library.id, detect_intros, enable_fingerprint)
            .unwrap();
    }

    pub fn comparison_runs(&self) -> usize {
        self.manager
            .comparison_runs
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn fingerprint_job_status(&self, episode: u32) -> String {
        let row = self.ledger_for_episode(episode);
        let store = self.store.lock();
        store
            .latest_probe_job_for_scope(&format!("ledger:{}", row.file.file_id))
            .unwrap()
            .map(|job| job.status)
            .unwrap_or_else(|| "none".to_string())
    }
}
