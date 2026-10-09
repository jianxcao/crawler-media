use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use api::{Store, ScrapeStoreExt};
use api::probe_manager::{ProbeManager, ProbeUnit};
use domain::{Confidence, LedgerId, LedgerRow, MediaId, MediaKind, QualitySource};
use marker::{CommonSegment, FingerprintEngine};
use marker::fingerprint::{CaptureFailure, CaptureMetrics, CaptureRequest, CapturedFingerprint, FingerprintCaptureEngine};
use parking_lot::Mutex;

struct FixtureEngine { case: &'static str, reads: Arc<AtomicUsize> }

fn intro_tag(case: &str, episode: u32) -> u32 {
    if case == "blind-seeds" && [2, 4, 7].contains(&episode) { 1000 + episode }
    else if (case == "other-variant" || case == "edge-variant") && [2, 4, 7].contains(&episode) { 1000 }
    else { 100 }
}

impl FingerprintEngine for FixtureEngine {
    fn extract_at(&self, _: &std::path::Path, _: u32, _: u32) -> Result<Vec<u32>, String> {
        panic!("this reproduction must not use legacy media extraction")
    }
    fn find_common_segment(&self, first: &[u32], second: &[u32], _: f32, _: f32) -> Option<CommonSegment> {
        if self.case == "version-conflict" && first.first() != Some(&5000) && second.first() != Some(&5000) {
            let start1 = if first.first() == Some(&1200) { 30.0 } else { 10.0 };
            let start2 = if second.first() == Some(&1200) { 30.0 } else { 10.0 };
            return Some(CommonSegment { start1_sec:start1, end1_sec:start1+60.0, start2_sec:start2, end2_sec:start2+60.0, duration_sec:60.0, score:1.0 });
        }
        if first.first() != second.first() || self.case == "no-outro" && first.first() == Some(&5000) { return None; }
        let end = if self.case == "edge-variant" && first.first() == Some(&100) { 180.0 } else { 70.0 };
        Some(CommonSegment { start1_sec: 10.0, end1_sec: end, start2_sec: 10.0,
            end2_sec: end, duration_sec: end-10.0, score: 1.0 })
    }
    fn find_common_segments(&self, first: &[u32], second: &[u32], min: f32, max: f32) -> Vec<CommonSegment> {
        self.find_common_segment(first, second, min, max).into_iter().collect()
    }
}

impl FingerprintCaptureEngine for FixtureEngine {
    fn capture_window(&self, request: &CaptureRequest) -> Result<CapturedFingerprint, CaptureFailure> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let stem = request.path.file_stem().unwrap().to_str().unwrap();
        let episode: u32 = stem.split('-').nth(1).unwrap().parse().unwrap();
        let version: usize = stem.rsplit('-').next().unwrap().parse().unwrap();
        let tag = if request.window.start_ms == 0 {
            if self.case == "version-conflict" && episode == 1 && version == 3 { 1200 }
            else if self.case == "mixed-versions" {
                if episode == 4 || (episode == 1 && version == 3) { 200 } else { 100 }
            } else { intro_tag(self.case, episode) }
        } else { 5000 };
        Ok(CapturedFingerprint { window: request.window.clone(), words: (tag..tag + 200).collect(),
            pcm_duration_ms: Some(request.window.duration_ms()), metrics: CaptureMetrics::default() })
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
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let media_id = MediaId::new();
    let engine = Arc::new(FixtureEngine { case, reads: Arc::new(AtomicUsize::new(0)) });
    let manager = ProbeManager::with_engines(store.clone(), engine.clone(), engine.clone());
    let mut rows: Vec<_> = episodes.iter().enumerate().map(|(index, &episode)| LedgerRow {
        id: LedgerId::new(), media_id,
        path: tmp.path().join(format!("episode-{episode}-version-{index}.mkv")).to_string_lossy().to_string(),
        season: Some(1), episode: Some(episode), resolution: None, codec: None, hdr: None,
        quality_source: QualitySource::Release, confidence: Confidence::High, filter_score: None,
    }).collect();
    if (case == "mixed-versions" || case == "version-conflict") && rows[0].id.to_string() > rows[3].id.to_string() {
        let original = rows[0].id;
        rows[0].id = rows[3].id;
        rows[3].id = original;
    }
    {
        let st = store.lock();
        let mut config = st.get_scrape_config().unwrap().setting;
        config.fingerprint_sampling_mode = Some("adaptive".into());
        st.save_scrape_config(&config).unwrap();
        st.insert_media(&domain::Media { id: media_id, kind: MediaKind::Tv, title: "Review fixture".into(),
            year: None, original_title: None, tmdb_id: None, douban_id: None, tvdb_id: None,
            bangumi_id: None, anilist_id: None }).unwrap();
        for row in &rows {
            st.insert_ledger(row).unwrap();
            st.put_file_meta_versioned(&row.id.to_string(),
                &library::Tracks { video: Some(library::VideoTrack::default()), audio: vec![], subtitles: vec![] },
                Some(&api::fingerprint_job::current_source_version(std::path::Path::new(&row.path))),
                Some(1_000_000)).unwrap();
            st.put_media_marker(&api::store::StoredMediaMarker { media_id, season: 1, episode: row.episode.unwrap(),
                intro_start_ms: Some(10_000), intro_end_ms: Some(70_000), outro_start_ms: None,
                outro_end_ms: None, source: "old".into(), locked: false, updated_at: 0 }).unwrap();
        }
        let keys: Vec<_> = rows.iter().map(|row| row.id.to_string()).collect();
        let specs: Vec<_> = keys.iter().map(|key| api::store::ProbeJobUnitSpec { ledger_id: key, kind: "tv",
            force_fingerprint: true, reuse_fingerprint_cache: false, overwrite_markers: true, reuse_media_info_cache: true }).collect();
        st.create_probe_job("review9-job", "marker_refresh", &media_id.to_string(), Some(1), "review9", &specs).unwrap();
        for key in &keys {
            st.start_probe_unit("review9-job", key).unwrap();
            st.finish_probe_unit("review9-job", key, true, None).unwrap();
        }
    }
    let units = rows.into_iter().map(|row| ProbeUnit { row, kind: MediaKind::Tv, force_fingerprint: true,
        reuse_fingerprint_cache: false, overwrite_markers: true, reuse_media_info_cache: true,
        marker_refresh_id: Some(1), job_id: Some("review9-job".into()) }).collect();
    Fixture { _tmp: tmp, store, manager, media_id, units, engine }
}

fn run(f: &Fixture) -> api::store::MarkerResultReplacement {
    let replacement = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap().block_on(
        api::probe_manager::season::run_adaptive_season_pipeline(
            &f.manager, "review9-job", f.media_id, 1, &f.units)).unwrap();
    debug_final_models(f);
    replacement
}

#[test]
fn control_uniform_season_publishes_all_markers() {
    let f = setup("uniform", &[1, 2, 3, 4]);
    let replacement = run(&f);
    assert_eq!(replacement.markers.iter().filter(|marker| marker.intro_start_ms.is_some()).count(), 4);
    f.store.lock().complete_marker_refresh("review9-job", &[replacement]).unwrap();
    assert_eq!(f.store.lock().get_probe_job("review9-job").unwrap().unwrap().status, "succeeded");
}

#[test]
fn duplicate_episode_versions_publish_without_unique_key_failure() {
    let f = setup("uniform", &[1, 1, 2, 3]);
    let replacement = run(&f);
    println!("multiple versions generated episode rows: {:?}", replacement.markers.iter().map(|marker| marker.episode).collect::<Vec<_>>());
    let result = f.store.lock().complete_marker_refresh("review9-job", &[replacement]);
    println!("publication: {result:?}");
    assert!(result.is_ok(), "a valid season with multiple versions of an episode must be publishable");
}

#[test]
fn full_samples_from_nonseeds_discover_the_common_intro() {
    let f = setup("blind-seeds", &[1, 2, 3, 4, 5, 6, 7, 8]);
    let known_intro: Vec<_> = f.units.iter().map(|unit| {
        let tag = intro_tag("blind-seeds", unit.row.episode.unwrap());
        (unit.row.clone(), (tag..tag + 200).collect())
    }).collect();
    let baseline = api::fingerprint_job::analyze_fingerprints_with_outro_engine(
        f.engine.as_ref(), f.media_id, 1, known_intro, Vec::new());
    let replacement = run(&f);
    let intro_count = replacement.markers.iter().filter(|marker| marker.intro_start_ms.is_some()).count();
    f.store.lock().complete_marker_refresh("review9-job", &[replacement]).unwrap();
    let after = f.store.lock().get_media_marker(f.media_id, Some(1), Some(1)).unwrap().unwrap();
    println!("baseline_intros={} adaptive_intros={intro_count} captures={} published_E1_intro={:?} job={}",
        baseline.len(), f.engine.reads.load(Ordering::Relaxed), after.intro_start_ms,
        f.store.lock().get_probe_job("review9-job").unwrap().unwrap().status);
    assert_eq!(baseline.len(), 5, "fixture must provide five independent matching episodes");
    assert_eq!(intro_count, baseline.len(), "full-window nonseed evidence must be analyzed before clearing old markers");
}

#[test]
fn unlocked_timeline_uses_the_known_media_duration() {
    let f = setup("no-outro", &[1, 2, 3]);
    let replacement = run(&f);
    f.store.lock().complete_marker_refresh("review9-job", &[replacement]).unwrap();
    let chapters = f.store.lock().get_cached_chapters(&f.units[0].row.id.to_string()).unwrap().unwrap();
    let feature = chapters.iter().find(|chapter| chapter.title.as_deref() == Some("正片")).unwrap();
    println!("known_duration_ms=1000000 feature_range=({}, {})", feature.start_ms, feature.end_ms);
    assert_eq!(feature.end_ms, 1_000_000, "unlocked chapter cache must end at known media duration");
}

#[test]
fn missing_duration_does_not_publish_a_successful_outro_deletion() {
    let f = setup("uniform", &[1, 2, 3]);
    let first = &f.units[0].row;
    {
        let st = f.store.lock();
        st.put_file_meta_versioned(&first.id.to_string(),
            &library::Tracks { video: Some(library::VideoTrack::default()), audio: vec![], subtitles: vec![] },
            Some(&api::fingerprint_job::current_source_version(std::path::Path::new(&first.path))), None).unwrap();
        let mut previous = st.get_media_marker(f.media_id, Some(1), Some(1)).unwrap().unwrap();
        previous.outro_start_ms = Some(930_000);
        previous.outro_end_ms = Some(990_000);
        st.put_media_marker(&previous).unwrap();
    }
    let replacement = run(&f);
    f.store.lock().complete_marker_refresh("review9-job", &[replacement]).unwrap();
    let after = f.store.lock().get_media_marker(f.media_id, Some(1), Some(1)).unwrap().unwrap();
    let status = f.store.lock().get_probe_job("review9-job").unwrap().unwrap().status;
    println!("unknown_E1_duration: job={status} captures={} old_outro=Some(930000) new_outro={:?}",
        f.engine.reads.load(Ordering::Relaxed), after.outro_start_ms);
    assert_eq!(after.outro_start_ms, Some(930_000), "no outro sample was taken; missing duration cannot confirm absence of an outro");
}

#[test]
fn another_common_variant_is_discovered_after_full_fallbacks() {
    let f = setup("other-variant", &[1, 2, 3, 4, 5, 6, 7, 8]);
    let known_intro: Vec<_> = f.units.iter().map(|unit| {
        let tag = intro_tag("other-variant", unit.row.episode.unwrap());
        (unit.row.clone(), (tag..tag + 200).collect())
    }).collect();
    let baseline = api::fingerprint_job::analyze_fingerprints_with_outro_engine(
        f.engine.as_ref(), f.media_id, 1, known_intro, Vec::new());
    let replacement = run(&f);
    let intro_episodes: Vec<_> = replacement.markers.iter().filter(|marker| marker.intro_start_ms.is_some()).map(|marker| marker.episode).collect();
    f.store.lock().complete_marker_refresh("review9-job", &[replacement]).unwrap();
    let after = f.store.lock().get_media_marker(f.media_id, Some(1), Some(1)).unwrap().unwrap();
    println!("two variants: baseline_intros={} adaptive_intros={intro_episodes:?} captures={} published_E1_intro={:?} job={}",
        baseline.len(), f.engine.reads.load(Ordering::Relaxed), after.intro_start_ms,
        f.store.lock().get_probe_job("review9-job").unwrap().unwrap().status);
    assert_eq!(baseline.len(), 8, "both three-episode and five-episode variants are independently supported");
    assert_eq!(intro_episodes.len(), baseline.len(), "nonseed fallback evidence must discover the second common variant");
}


#[test]
fn new_variant_with_inconclusive_boundary_does_not_clear_old_intro() {
    let f = setup("edge-variant", &[1, 2, 3, 4, 5, 6, 7, 8]);
    let replacement = run(&f);
    let intro_episodes: Vec<_> = replacement.markers.iter().filter(|m| m.intro_start_ms.is_some()).map(|m| m.episode).collect();
    f.store.lock().complete_marker_refresh("review9-job", &[replacement]).unwrap();
    let after = f.store.lock().get_media_marker(f.media_id, Some(1), Some(1)).unwrap().unwrap();
    println!("edge variant intro episodes={intro_episodes:?} E1_intro={:?} status={}", after.intro_start_ms,
        f.store.lock().get_probe_job("review9-job").unwrap().unwrap().status);
    assert_eq!(after.intro_start_ms, Some(10_000), "inconclusive template boundary must not publish confirmed no-match and clear old intro");
}

#[test]
fn two_versions_of_one_episode_do_not_merge_distinct_intro_models() {
    let f = setup("mixed-versions", &[1, 2, 3, 1, 4]);
    let replacement = run(&f);
    println!("mixed versions markers={:?}", replacement.markers.iter().map(|m| (m.episode,m.intro_start_ms)).collect::<Vec<_>>());
    let result = replacement.markers.iter().find(|m| m.episode == 4).unwrap();
    assert_eq!(result.intro_start_ms, Some(10_000), "E4 matches E1's second version and should have an independently supported model");
}

#[test]
fn missing_duration_preserves_start_only_outro_marker() {
    let f = setup("uniform", &[1, 2, 3]);
    let first = &f.units[0].row;
    {
        let st = f.store.lock();
        st.put_file_meta_versioned(&first.id.to_string(),
            &library::Tracks { video: Some(library::VideoTrack::default()), audio: vec![], subtitles: vec![] },
            Some(&api::fingerprint_job::current_source_version(std::path::Path::new(&first.path))), None).unwrap();
        let mut previous = st.get_media_marker(f.media_id, Some(1), Some(1)).unwrap().unwrap();
        previous.outro_start_ms = Some(930_000);
        previous.outro_end_ms = None;
        st.put_media_marker(&previous).unwrap();
    }
    let replacement = run(&f);
    f.store.lock().complete_marker_refresh("review9-job", &[replacement]).unwrap();
    let after = f.store.lock().get_media_marker(f.media_id, Some(1), Some(1)).unwrap().unwrap();
    println!("unknown duration + start-only outro={:?}/{:?}", after.outro_start_ms, after.outro_end_ms);
    assert_eq!(after.outro_start_ms, Some(930_000), "unsampled valid credits start must be preserved");
}


#[test]
fn conflicting_version_boundaries_do_not_publish_one_versions_range_for_both() {
    let f = setup("version-conflict", &[1, 2, 3, 1]);
    let result = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap().block_on(
        api::probe_manager::season::run_adaptive_season_pipeline(&f.manager, "review9-job", f.media_id, 1, &f.units));
    let is_conflict = result.as_ref().err().map(|e| e.contains("episode_version_conflict")).unwrap_or(false);
    if let Ok(replacement) = result {
        let ranges: Vec<_> = replacement.chapter_updates.iter().map(|(id,chs)| (
            id.clone(), chs.iter().find(|c| c.marker_type == Some(marker::MarkerType::IntroStart)).map(|c| (c.start_ms,c.end_ms)))).collect();
        println!("conflicting ledger chapter intros={ranges:?}");
        f.store.lock().complete_marker_refresh("review9-job", &[replacement]).unwrap();
        let canonical=f.store.lock().get_media_marker(f.media_id,Some(1),Some(1)).unwrap().unwrap();
        println!("canonical E1 intro {:?}..{:?} job={}",canonical.intro_start_ms,canonical.intro_end_ms,
            f.store.lock().get_probe_job("review9-job").unwrap().unwrap().status);
    }
    assert!(is_conflict, "different boundaries for one logical episode must fail atomically with episode_version_conflict");
}


fn debug_final_models(f: &Fixture) {
    use marker::adaptive::{SamplingPolicy, SegmentKind, TemplateContext, TemplateModel};
    if !["edge-variant","mixed-versions"].contains(&f.engine.case) { return; }
    let profile = api::fingerprint_job::capture_profile_key(&api::fingerprint_job::FingerprintCaptureProfile::default());
    let st = f.store.lock();
    let mut evidence = Vec::new();
    for unit in &f.units {
        let samples = st.find_covering_fingerprint_samples(&api::store::FingerprintSampleQuery {
            ledger_id: unit.row.id.to_string(),
            source_version: api::fingerprint_job::current_source_version(std::path::Path::new(&unit.row.path)),
            capture_profile_key: profile.clone(), kind:"intro".into(), window_start_ms:0,window_end_ms:1,
            captured_job_id:Some("review9-job".into()) }).unwrap();
        if let Some(sample) = samples.iter().max_by_key(|s| s.window_end_ms-s.window_start_ms) {
            evidence.push(api::fingerprint_job::adaptive::stored_sample_to_evidence(
                sample, unit.row.episode.unwrap(),SegmentKind::Intro,Some(1_000_000)));
        }
    }
    let models:Vec<TemplateModel> = st.list_fingerprint_models(&f.media_id.to_string(),1).unwrap()
        .into_iter().filter(|m| m.kind=="intro").map(|m| serde_json::from_str(&m.model_json).unwrap()).collect();
    let ctx=TemplateContext { models, references:evidence.iter().map(|e|(e.sample_id.clone(),e.clone())).collect() };
    let target_ep=if f.engine.case=="edge-variant" {1} else {4};
    let target=evidence.iter().find(|e| e.episode==target_ep).unwrap();
    println!("{} final template refs={:?} E{} verification={:?}",f.engine.case,
        ctx.models.iter().map(|m|m.references.iter().map(|r|r.episode).collect::<Vec<_>>()).collect::<Vec<_>>(),target_ep,
        marker::adaptive::verify_template_window(f.engine.as_ref(),target,&ctx,&SamplingPolicy::default()));
    if f.engine.case=="mixed-versions" {
        let variant:Vec<_>=evidence.iter().filter(|e|e.capture.words.first()==Some(&200)).cloned().collect();
        let models=marker::adaptive::build_season_models(f.engine.as_ref(),&variant,&SamplingPolicy::default());
        let independent=TemplateContext { models, references:variant.iter().map(|e|(e.sample_id.clone(),e.clone())).collect() };
        println!("isolated B variant E4 verification={:?}",marker::adaptive::verify_template_window(f.engine.as_ref(),target,&independent,&SamplingPolicy::default()));
    }
}
