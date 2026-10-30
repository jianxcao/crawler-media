use api::probe_manager::{ProbeManager, ProbeUnit};
use api::{ScrapeStoreExt, Store};
use domain::{Confidence, LedgerId, MediaId, MediaKind, QualitySource};
use marker::fingerprint::{
    CaptureFailure, CaptureMetrics, CaptureRequest, CapturedFingerprint, FingerprintCaptureEngine,
};
use marker::{CommonSegment, FingerprintEngine};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use store::test_support::{FixtureFile, publish_fixture_file};

#[path = "support/marker_timeline.rs"]
mod marker_timeline;

struct FixtureEngine {
    case: &'static str,
    reads: Arc<AtomicUsize>,
}

fn intro_tag(case: &str, episode: u32) -> u32 {
    if case == "blind-seeds" && [2, 4, 7].contains(&episode) {
        1000 + episode
    } else if case == "other-variant" && [2, 4, 7].contains(&episode) {
        1000
    } else {
        100
    }
}

impl FingerprintEngine for FixtureEngine {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _control: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        panic!("this reproduction must not use legacy media extraction")
    }
    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        _: f32,
        _: f32,
    ) -> Option<CommonSegment> {
        if first.first() != second.first() {
            return None;
        }
        Some(CommonSegment {
            start1_sec: 10.0,
            end1_sec: 70.0,
            start2_sec: 10.0,
            end2_sec: 70.0,
            duration_sec: 60.0,
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
        let tag = if request.window.start_ms == 0 {
            intro_tag(self.case, episode)
        } else if self.case == "no-outro" && episode == 1 {
            6000
        } else {
            5000
        };
        Ok(CapturedFingerprint {
            window: request.window.clone(),
            words: (tag..tag + 200).collect(),
            pcm_duration_ms: Some(request.window.duration_ms()),
            metrics: CaptureMetrics::default(),
        })
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    store: Arc<Store>,
    manager: ProbeManager,
    media_id: MediaId,
    units: Vec<ProbeUnit>,
    engine: Arc<FixtureEngine>,
}

fn setup(case: &'static str, episodes: &[u32]) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path()).unwrap());
    let media_id = MediaId::new();
    let engine = Arc::new(FixtureEngine {
        case,
        reads: Arc::new(AtomicUsize::new(0)),
    });
    let manager =
        ProbeManager::with_engines(store.clone(), engine.clone(), engine.clone()).unwrap();
    let tv_root = store.as_ref().library_root(MediaKind::Tv).unwrap();
    let rows: Vec<_> = episodes
        .iter()
        .enumerate()
        .map(|(index, &episode)| FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id,
            path: tv_root
                .join(format!("episode-{episode}-version-{index}.mkv"))
                .to_string_lossy()
                .to_string(),
            season: Some(1),
            episode: Some(episode),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            ..Default::default()
        })
        .collect();
    let coverages = seed_store(&store, media_id, &rows);
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

fn try_run(f: &Fixture) -> Result<api::store::MarkerResultReplacement, String> {
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
fn run(f: &Fixture) -> api::store::MarkerResultReplacement {
    try_run(f).expect("the actual pipeline must succeed")
}

#[test]
fn control_uniform_season_publishes_all_markers() {
    let f = setup("uniform", &[1, 2, 3, 4]);
    let replacement = run(&f);
    assert_eq!(
        replacement
            .markers
            .iter()
            .filter(|marker| marker.intro_start_ms.is_some())
            .count(),
        4
    );
    f.store
        .as_ref()
        .complete_marker_refresh("review9-job", &[replacement])
        .unwrap();
    assert_eq!(
        f.store
            .as_ref()
            .get_probe_job("review9-job")
            .unwrap()
            .unwrap()
            .status,
        "succeeded"
    );
}

#[test]
fn duplicate_episode_versions_publish_without_unique_key_failure() {
    let f = setup("uniform", &[1, 1, 2, 3]);
    let replacement = run(&f);
    println!(
        "multiple versions generated episode rows: {:?}",
        replacement
            .markers
            .iter()
            .map(|marker| marker.episode)
            .collect::<Vec<_>>()
    );
    let result = f
        .store
        .as_ref()
        .complete_marker_refresh("review9-job", &[replacement]);
    println!("publication: {result:?}");
    assert!(
        result.is_ok(),
        "a valid season with multiple versions of an episode must be publishable"
    );
}

#[test]
fn full_samples_from_nonseeds_discover_the_common_intro() {
    let f = setup("blind-seeds", &[1, 2, 3, 4, 5, 6, 7, 8]);
    let known_intro: Vec<_> = f
        .units
        .iter()
        .map(|unit| {
            let ep = unit
                .row
                .units
                .first()
                .map(|u| u.episode as u32)
                .unwrap_or(1);
            let tag = intro_tag("blind-seeds", ep);
            (unit.row.clone(), (tag..tag + 200).collect())
        })
        .collect();
    let baseline = api::fingerprint_job::analyze_fingerprints_with_outro_engine(
        f.engine.as_ref(),
        f.media_id,
        1,
        known_intro,
        Vec::new(),
    )
    .unwrap();
    let replacement = run(&f);
    let intro_count = replacement
        .markers
        .iter()
        .filter(|marker| marker.intro_start_ms.is_some())
        .count();
    f.store
        .as_ref()
        .complete_marker_refresh("review9-job", &[replacement])
        .unwrap();
    let after = f
        .store
        .as_ref()
        .get_media_marker(f.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    println!(
        "baseline_intros={} adaptive_intros={intro_count} captures={} published_E1_intro={:?} job={}",
        baseline.len(),
        f.engine.reads.load(Ordering::Relaxed),
        after.intro_start_ms,
        f.store
            .as_ref()
            .get_probe_job("review9-job")
            .unwrap()
            .unwrap()
            .status
    );
    assert_eq!(
        baseline.len(),
        5,
        "fixture must provide five independent matching episodes"
    );
    assert_eq!(
        intro_count,
        baseline.len(),
        "full-window nonseed evidence must be analyzed before clearing old markers"
    );
}

#[test]
fn unlocked_timeline_uses_the_known_media_duration() {
    let f = setup("no-outro", &[1, 2, 3]);
    let replacement = run(&f);
    f.store
        .as_ref()
        .complete_marker_refresh("review9-job", &[replacement])
        .unwrap();
    let chapters =
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(marker_timeline::resolved_chapters(
                f.store.clone(),
                f._tmp.path(),
                &f.units[0].row,
            ));
    let feature = chapters
        .iter()
        .find(|chapter| chapter.title.as_deref() == Some("正片"))
        .unwrap();
    println!(
        "known_duration_ms=1000000 feature_range=({}, {})",
        feature.start_ms, feature.end_ms
    );
    assert_eq!(
        feature.end_ms, 1_000_000,
        "resolved chapter timeline must end at known media duration"
    );
}

#[test]
fn missing_duration_does_not_publish_a_successful_outro_deletion() {
    let f = setup("uniform", &[1, 2, 3]);
    let first = &f.units[0].row;
    {
        let st = f.store.as_ref();
        let version = st
            .get_file_version(first.file.file_id)
            .unwrap()
            .expect("published row must carry a file version")
            .source_version;
        st.put_file_meta_versioned(
            &first.file.file_id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack::default()),
                audio: vec![],
                subtitles: vec![],
            },
            Some(&version),
            None,
        )
        .unwrap();
        let mut previous = st
            .get_media_marker(f.media_id, Some(1), Some(1))
            .unwrap()
            .unwrap();
        previous.outro_start_ms = Some(930_000);
        previous.outro_end_ms = Some(990_000);
        st.put_media_marker(&previous).unwrap();
    }
    assert!(try_run(&f).is_err());
    let after = f
        .store
        .as_ref()
        .get_media_marker(f.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    let status = f
        .store
        .as_ref()
        .get_probe_job("review9-job")
        .unwrap()
        .unwrap()
        .status;
    println!(
        "unknown_E1_duration: job={status} captures={} old_outro=Some(930000) new_outro={:?}",
        f.engine.reads.load(Ordering::Relaxed),
        after.outro_start_ms
    );
    assert_eq!(
        after.outro_start_ms,
        Some(930_000),
        "no outro sample was taken; missing duration cannot confirm absence of an outro"
    );
}

#[test]
fn another_common_variant_is_discovered_after_full_fallbacks() {
    let f = setup("other-variant", &[1, 2, 3, 4, 5, 6, 7, 8]);
    let known_intro: Vec<_> = f
        .units
        .iter()
        .map(|unit| {
            let ep = unit
                .row
                .units
                .first()
                .map(|u| u.episode as u32)
                .unwrap_or(1);
            let tag = intro_tag("other-variant", ep);
            (unit.row.clone(), (tag..tag + 200).collect())
        })
        .collect();
    let baseline = api::fingerprint_job::analyze_fingerprints_with_outro_engine(
        f.engine.as_ref(),
        f.media_id,
        1,
        known_intro,
        Vec::new(),
    )
    .unwrap();
    let replacement = run(&f);
    let intro_episodes: Vec<_> = replacement
        .markers
        .iter()
        .filter(|marker| marker.intro_start_ms.is_some())
        .map(|marker| marker.episode)
        .collect();
    f.store
        .as_ref()
        .complete_marker_refresh("review9-job", &[replacement])
        .unwrap();
    let after = f
        .store
        .as_ref()
        .get_media_marker(f.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    println!(
        "two variants: baseline_intros={} adaptive_intros={intro_episodes:?} captures={} published_E1_intro={:?} job={}",
        baseline.len(),
        f.engine.reads.load(Ordering::Relaxed),
        after.intro_start_ms,
        f.store
            .as_ref()
            .get_probe_job("review9-job")
            .unwrap()
            .unwrap()
            .status
    );
    assert_eq!(
        baseline.len(),
        8,
        "both three-episode and five-episode variants are independently supported"
    );
    assert_eq!(
        intro_episodes.len(),
        baseline.len(),
        "nonseed fallback evidence must discover the second common variant"
    );
}

fn seed_probe(store: &Store, row: &domain::FileCoverage, duration_ms: Option<i64>) {
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
        hdr: store::library_write::probe::Observed::Value(hdr.as_deref()),
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

fn seed_store(
    store: &Arc<Store>,
    media_id: MediaId,
    rows: &[FixtureFile],
) -> Vec<domain::FileCoverage> {
    let st = store.as_ref();
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
    let mut coverages = Vec::new();
    for row in rows {
        std::fs::write(&row.path, b"dummy").unwrap();
        let file = publish_fixture_file(st, row).unwrap();
        let coverage = domain::FileCoverage {
            path_key: String::new(),
            file,
            units: match (row.season, row.episode) {
                (Some(s), Some(e)) => vec![domain::UnitKey::new(s as i32, e as i32)],
                _ => vec![domain::UnitKey::WHOLE],
            },
        };
        seed_probe(st, &coverage, Some(1_000_000));
        st.put_media_marker(&api::store::StoredMediaMarker {
            media_id,
            season: 1,
            episode: row.episode.unwrap(),
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
        .map(|c| c.file.file_id.to_string())
        .collect();
    let specs: Vec<_> = keys
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
        &specs,
    )
    .unwrap();
    for key in &keys {
        st.start_probe_unit("review9-job", key).unwrap();
        st.finish_probe_unit("review9-job", key, true, None)
            .unwrap();
    }
    coverages
}
