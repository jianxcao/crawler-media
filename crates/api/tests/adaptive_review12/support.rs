use api::probe_manager::{ProbeManager, ProbeUnit};
use api::{ScrapeStoreExt, Store};
use domain::{
    Confidence, FileCoverage, LedgerId, LibraryFile, MediaId, MediaKind, QualitySource, UnitKey,
};
use marker::fingerprint::{
    CaptureFailure, CaptureMetrics, CaptureRequest, CapturedFingerprint, FingerprintCaptureEngine,
};
use marker::{CommonSegment, FingerprintEngine};
use parking_lot::Mutex;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use store::test_support::{FixtureFile, publish_fixture_file};

struct FixtureEngine {
    case: &'static str,
    reads: Arc<AtomicUsize>,
}

fn intro_tag(case: &str, episode: u32) -> u32 {
    if case == "blind-seeds" && [2, 4, 7].contains(&episode) {
        1000 + episode
    } else if (case == "other-variant" || case == "edge-variant") && [2, 4, 7].contains(&episode) {
        1000
    } else {
        100
    }
}

impl FingerprintEngine for FixtureEngine {
    fn extract_at(&self, _: &std::path::Path, _: u32, _: u32) -> Result<Vec<u32>, String> {
        panic!("this reproduction must not use legacy media extraction")
    }
    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        _: f32,
        _: f32,
    ) -> Option<CommonSegment> {
        if ["vote-duplication", "ambiguous-final"].contains(&self.case)
            && first.first() != Some(&5000)
            && second.first() != Some(&5000)
        {
            let other = if first.first() == Some(&104) {
                second[0]
            } else {
                first[0]
            };
            let target_start = if other == 101 || other == 105 {
                10.0
            } else {
                100.0
            };
            let start1 = if first.first() == Some(&104) {
                target_start
            } else {
                10.0
            };
            let start2 = if second.first() == Some(&104) {
                target_start
            } else {
                10.0
            };
            if start1 + 60.0 > first.len() as f32 || start2 + 60.0 > second.len() as f32 {
                return None;
            }
            return Some(CommonSegment {
                start1_sec: start1,
                end1_sec: start1 + 60.0,
                start2_sec: start2,
                end2_sec: start2 + 60.0,
                duration_sec: 60.0,
                score: 1.0,
            });
        }
        if self.case == "version-conflict"
            && first.first() != Some(&5000)
            && second.first() != Some(&5000)
        {
            let start1 = if first.first() == Some(&1200) {
                30.0
            } else {
                10.0
            };
            let start2 = if second.first() == Some(&1200) {
                30.0
            } else {
                10.0
            };
            return Some(CommonSegment {
                start1_sec: start1,
                end1_sec: start1 + 60.0,
                start2_sec: start2,
                end2_sec: start2 + 60.0,
                duration_sec: 60.0,
                score: 1.0,
            });
        }
        if first.first() != second.first()
            || self.case == "no-outro" && first.first() == Some(&5000)
        {
            return None;
        }
        let end = if self.case == "edge-variant" && first.first() == Some(&100) {
            180.0
        } else {
            70.0
        };
        Some(CommonSegment {
            start1_sec: 10.0,
            end1_sec: end,
            start2_sec: 10.0,
            end2_sec: end,
            duration_sec: end - 10.0,
            score: 1.0,
        })
    }
    fn find_common_segments(
        &self,
        first: &[u32],
        second: &[u32],
        min: f32,
        max: f32,
    ) -> Vec<CommonSegment> {
        self.find_common_segment(first, second, min, max)
            .into_iter()
            .collect()
    }
}

impl FingerprintCaptureEngine for FixtureEngine {
    fn capture_window(
        &self,
        request: &CaptureRequest,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let stem = request.path.file_stem().unwrap().to_str().unwrap();
        let episode: u32 = stem.split('-').nth(1).unwrap().parse().unwrap();
        let version: usize = stem.rsplit('-').next().unwrap().parse().unwrap();
        let tag = if request.window.start_ms == 0 {
            if ["vote-duplication", "ambiguous-final"].contains(&self.case) {
                100 + episode
            } else if (self.case == "version-conflict" || self.case == "version-missing")
                && episode == 1
                && version == 3
            {
                1200
            } else if self.case == "mixed-versions" {
                if episode == 4 || (episode == 1 && version == 3) {
                    200
                } else {
                    100
                }
            } else {
                intro_tag(self.case, episode)
            }
        } else if self.case == "version-missing" && episode == 1 && version == 3 {
            6200
        } else {
            5000
        };
        Ok(CapturedFingerprint {
            window: request.window.clone(),
            words: (tag..tag
                + if ["vote-duplication", "ambiguous-final"].contains(&self.case) {
                    (request.window.duration_ms() / 1000) as u32
                } else {
                    200
                })
                .collect(),
            pcm_duration_ms: Some(request.window.duration_ms()),
            metrics: CaptureMetrics::default(),
        })
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    store: Arc<Mutex<Store>>,
    manager: ProbeManager,
    media_id: MediaId,
    units: Vec<ProbeUnit>,
    engine: Arc<FixtureEngine>,
}

fn setup(case: &'static str, episodes: &[u32]) -> Fixture {
    setup_with_versions(case, episodes, false)
}

/// Publish rows whose stored source version is the digest the probe recomputes from the
/// file, so media-info cache reuse and probe publication accept them.
fn setup_with_digest_versions(case: &'static str, episodes: &[u32]) -> Fixture {
    setup_with_versions(case, episodes, true)
}

fn setup_with_versions(case: &'static str, episodes: &[u32], digest_versions: bool) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let media_id = MediaId::new();
    let engine = Arc::new(FixtureEngine {
        case,
        reads: Arc::new(AtomicUsize::new(0)),
    });
    let manager = ProbeManager::with_engines(store.clone(), engine.clone(), engine.clone());
    let tv_root = store.lock().library_root(MediaKind::Tv).unwrap();
    let mut specs: Vec<FixtureFile> = episodes
        .iter()
        .enumerate()
        .map(|(index, &episode)| FixtureFile {
            media_id,
            path: tv_root
                .join(format!("episode-{episode}-version-{index}.mkv"))
                .to_string_lossy()
                .to_string(),
            season: Some(1),
            episode: Some(episode),
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            file_id: Some(LedgerId::new()),
            ..FixtureFile::default()
        })
        .collect();
    if (case == "mixed-versions" || case == "version-conflict" || case == "version-missing")
        && specs[0].file_id.unwrap().to_string() > specs[3].file_id.unwrap().to_string()
    {
        let original = specs[0].file_id;
        specs[0].file_id = specs[3].file_id;
        specs[3].file_id = original;
    }
    let coverages = seed_store(&store, media_id, &specs, digest_versions);
    let units = coverages
        .into_iter()
        .map(|row| ProbeUnit {
            row,
            kind: MediaKind::Tv,
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: true,
            marker_refresh_id: Some(1),
            job_id: Some("review9-job".into()),
        })
        .collect();
    Fixture {
        _tmp: tmp,
        store,
        manager,
        media_id,
        units,
        engine,
    }
}

fn raw_season(f: &Fixture) -> Result<api::store::MarkerResultReplacement, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(api::probe_manager::season::run_adaptive_season_pipeline(
            &f.manager,
            "review9-job",
            f.media_id,
            1,
            &f.units,
        ))
}

fn seed_probe(store: &Store, row: &FileCoverage, duration_ms: Option<i64>) {
    let expected = store
        .get_file_version(row.file.file_id)
        .unwrap()
        .expect("published row must carry a file version");
    let tracks = library::Tracks {
        video: Some(library::VideoTrack::default()),
        audio: vec![],
        subtitles: vec![],
    };
    let (resolution, codec, hdr) = match tracks.video.as_ref() {
        Some(video) => (video.resolution(), video.codec.clone(), video.hdr()),
        None => (None, None, None),
    };
    let details = api::store::files::FileDetails {
        source_revision: expected.revision,
        source_version: expected.source_version.clone(),
        tracks_version: api::store::library_write::probe::CURRENT_TRACKS_VERSION,
        video_json: tracks
            .video
            .as_ref()
            .and_then(|video| serde_json::to_string(video).ok()),
        audio_json: serde_json::to_string(&tracks.audio).unwrap(),
        subtitle_json: serde_json::to_string(&tracks.subtitles).unwrap(),
        chapters_json: None,
        fingerprint_json: None,
        outro_fingerprint_json: None,
        format_duration_ms: duration_ms,
    };
    let publication = api::store::library_write::probe::ProbePublication {
        expected: &expected,
        tracks_version: api::store::library_write::probe::CURRENT_TRACKS_VERSION,
        resolution: resolution.as_deref(),
        codec: codec.as_deref(),
        hdr: hdr.as_deref(),
        duration_ms,
        details: Some(&details),
    };
    assert!(
        store
            .publish_probe_result(row.file.file_id, &publication, 0)
            .unwrap(),
        "probe publication must be accepted for a freshly published row"
    );
}

/// Publish one fixture file with an explicit source version, exactly as a scan does.
fn publish_row_with_version(
    store: &Store,
    spec: &FixtureFile,
    source_version: &str,
) -> LibraryFile {
    let media = store
        .get_media(spec.media_id)
        .unwrap()
        .expect("fixture media must exist");
    let path = std::path::Path::new(&spec.path);
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
        file_id: spec.file_id.expect("fixture spec must carry a file id"),
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
    let units = spec_units(spec);
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
    file
}

/// The unit coordinates a fixture spec publishes under, mirroring the store's own
/// spec-to-units mapping.
fn spec_units(spec: &FixtureFile) -> Vec<UnitKey> {
    match (spec.season, spec.episode) {
        (Some(season), Some(episode)) => vec![UnitKey::new(season as i32, episode as i32)],
        _ => vec![UnitKey::WHOLE],
    }
}

fn seed_store(
    store: &Arc<Mutex<Store>>,
    media_id: MediaId,
    specs: &[FixtureFile],
    digest_versions: bool,
) -> Vec<FileCoverage> {
    let st = store.lock();
    let mut config = st.get_scrape_config().unwrap().setting;
    config.fingerprint_sampling_mode = Some("adaptive".into());
    st.save_scrape_config(&config).unwrap();
    st.insert_media(&domain::Media {
        id: media_id,
        kind: MediaKind::Tv,
        title: "Review fixture".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    })
    .unwrap();
    let mut coverages = Vec::with_capacity(specs.len());
    for spec in specs {
        std::fs::write(&spec.path, b"dummy").unwrap();
        let file = if digest_versions {
            let source_version =
                api::fingerprint_job::current_source_version(std::path::Path::new(&spec.path));
            publish_row_with_version(&st, spec, &source_version)
        } else {
            publish_fixture_file(&st, spec).unwrap()
        };
        let coverage = FileCoverage {
            path_key: String::new(),
            file,
            units: spec_units(spec),
        };
        seed_probe(&st, &coverage, Some(1_000_000));
        st.put_media_marker(&api::store::StoredMediaMarker {
            media_id,
            season: 1,
            episode: spec.episode.expect("fixture spec must be episodic"),
            intro_start_ms: Some(10_000),
            intro_end_ms: Some(70_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "old".into(),
            locked: false,
            updated_at: 0,
        })
        .unwrap();
        coverages.push(coverage);
    }
    let keys: Vec<_> = coverages
        .iter()
        .map(|coverage| coverage.file.file_id.to_string())
        .collect();
    let unit_specs: Vec<_> = keys
        .iter()
        .map(|key| api::store::ProbeJobUnitSpec {
            ledger_id: key,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        })
        .collect();
    st.create_probe_job(
        "review9-job",
        "marker_refresh",
        &media_id.to_string(),
        Some(1),
        "review9",
        &unit_specs,
    )
    .unwrap();
    for key in &keys {
        st.start_probe_unit("review9-job", key).unwrap();
        st.finish_probe_unit("review9-job", key, true, None)
            .unwrap();
    }
    coverages
}
