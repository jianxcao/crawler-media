use super::*;

#[derive(Clone, Copy)]
enum Case {
    Ambiguous,
    Insufficient,
    TruncatedNoMatch,
    CompleteNoMatch,
}
struct ReviewEngine(Case);
impl FingerprintEngine for ReviewEngine {
    fn compare_fingerprints(
        &self,
        first: &[u32],
        second: &[u32],
        min: f32,
        max: f32,
    ) -> marker::PairFingerprintDecision {
        let candidates = self.find_common_segments(first, second, min, max);
        if candidates.is_empty() {
            marker::PairFingerprintDecision::NoMatch
        } else {
            marker::PairFingerprintDecision::Candidates(candidates)
        }
    }

    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!("capture is explicitly injected")
    }
    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        min: f32,
        max: f32,
    ) -> Option<CommonSegment> {
        self.find_common_segments(first, second, min, max)
            .into_iter()
            .next()
    }
    fn find_common_segments(
        &self,
        first: &[u32],
        second: &[u32],
        min: f32,
        max: f32,
    ) -> Vec<CommonSegment> {
        let intro = first[0] < 100;
        if intro {
            let target = first[0] == 1 || second[0] == 1;
            let other = first[0].max(second[0]);
            if target && matches!(self.0, Case::TruncatedNoMatch | Case::CompleteNoMatch)
                || target && matches!(self.0, Case::Insufficient) && other != 2
            {
                return vec![];
            }
        }
        let candidate = FixedCandidate
            .find_common_segment(first, second, min, max)
            .unwrap();
        if intro && matches!(self.0, Case::Ambiguous) {
            vec![
                candidate,
                CommonSegment {
                    start1_sec: 110.0,
                    end1_sec: 170.0,
                    start2_sec: 110.0,
                    end2_sec: 170.0,
                    duration_sec: 60.0,
                    score: 0.0,
                },
            ]
        } else {
            vec![candidate]
        }
    }
}
impl marker::FingerprintCaptureEngine for ReviewEngine {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        let name = request.path.file_stem().unwrap().to_str().unwrap();
        let episode = name.trim_start_matches("S01E0").parse::<u32>().unwrap();
        let tag = episode
            + if request.window.start_ms < 600_000 {
                0
            } else {
                100
            };
        let mut captured = FixedCandidate.capture_window(request)?;
        captured.words = vec![tag, tag + 10];
        if tag == 1 && matches!(self.0, Case::TruncatedNoMatch) {
            captured.pcm_duration_ms = Some(request.window.duration_ms() - 500);
        }
        Ok(captured)
    }
}
async fn review_scenario(case: Case, mode: &str) -> ProbeScenario {
    use api::ScrapeStoreExt;
    let mut s = scenario().await;
    s.manager.shutdown_workers().await.unwrap();
    let engine = Arc::new(ReviewEngine(case));
    s.manager =
        Arc::new(ProbeManager::with_engines(s.store.clone(), engine.clone(), engine).unwrap());
    let mut config = s.store.get_scrape_config().unwrap().setting;
    config.fingerprint_sampling_mode = Some(mode.into());
    s.store.save_scrape_config(&config).unwrap();
    for (index, row) in s.ledgers.iter().enumerate() {
        let key = row.file.file_id.to_string();
        let mut cache = s.store.get_fingerprint_cache(&key).unwrap().unwrap();
        let tag = index as u32 + 1;
        cache.intro.words = vec![tag, tag + 10];
        cache.outro.as_mut().unwrap().words = vec![tag + 100, tag + 110];
        s.store.put_fingerprint_cache(&key, &cache).unwrap();
    }
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
    s
}
fn old_marker(s: &ProbeScenario) -> store::StoredFileMarker {
    let old = store::StoredFileMarker {
        intro_start_ms: Some(11_000),
        intro_end_ms: Some(80_000),
        outro_start_ms: Some(2_300_000),
        outro_end_ms: Some(2_350_000),
        source: "fingerprint".into(),
        locked: false,
        updated_at: 1,
        ..store::test_support::fixture_marker(&s.store, s.media_id, domain::UnitKey::new(1, 1))
    };
    s.store.put_fixture_marker(&old).unwrap();
    old
}
fn assert_old(s: &ProbeScenario, old: &store::StoredFileMarker) {
    let after = s
        .store
        .get_fixture_marker(s.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(
        (after.intro_start_ms, after.intro_end_ms),
        (old.intro_start_ms, old.intro_end_ms)
    );
    assert_eq!(
        (after.outro_start_ms, after.outro_end_ms),
        (old.outro_start_ms, old.outro_end_ms)
    );
}
async fn refresh(s: &ProbeScenario) -> store::ProbeJob {
    assert_eq!(
        s.manager
            .enqueue_marker_refresh(s.ledgers.iter().map(|r| unit(r, true)).collect()),
        s.ledgers.len()
    );
    let job = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(job) = s
                .store
                .latest_marker_refresh_for_season(&s.media_id.to_string(), 1)
                .unwrap()
                && !job.is_active()
            {
                break job;
            }
            advance_analysis(s);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    s.manager.shutdown_workers().await.unwrap();
    job
}
fn assert_ordinary_uncertain(s: &ProbeScenario) {
    let row = &s.ledgers[0];
    let job = s
        .store
        .latest_probe_job_for_scope(&format!("ledger:{}", row.file.file_id))
        .unwrap()
        .unwrap();
    let units = s.store.probe_job_units(&job.id).unwrap();
    let outcome: serde_json::Value = serde_json::from_str(
        units[0]
            .detection_outcome_json
            .as_deref()
            .expect("unresolved decision must persist"),
    )
    .unwrap();
    assert_eq!(outcome["status"], "uncertain");
    assert!(
        outcome["reason"]
            .as_str()
            .unwrap()
            .contains("verification_unresolved")
    );
    assert_eq!(s.manager.dispatch_due(i64::MAX / 2, 8).unwrap(), 0);
}
fn assert_preserved_intro_verified_outro(s: &ProbeScenario, old: &store::StoredFileMarker) {
    let after = s
        .store
        .get_file_marker(old.file_id, old.unit)
        .unwrap()
        .unwrap();
    assert_eq!(
        (after.intro_start_ms, after.intro_end_ms),
        (old.intro_start_ms, old.intro_end_ms)
    );
    assert_eq!(
        (after.outro_start_ms, after.outro_end_ms),
        (Some(2_250_000), Some(2_310_000))
    );
    let outcome = latest_outcome(s);
    assert_eq!(outcome["status"], "partial");
    assert_eq!(outcome["kinds"]["intro"]["status"], "uncertain");
    assert_eq!(outcome["kinds"]["outro"]["status"], "verified");
    assert!(
        outcome["kinds"]["intro"]["reason"]
            .as_str()
            .unwrap()
            .contains("verification_unresolved")
    );
}
async fn unresolved_fullwindow(case: Case, manual: bool) {
    let s = review_scenario(case, "full_window").await;
    let old = old_marker(&s);
    if manual {
        assert_eq!(refresh(&s).await.status, "succeeded");
    } else {
        probe(&s).await;
    }
    assert_preserved_intro_verified_outro(&s, &old);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ambiguous_fullwindow_refresh_preserves_previous() {
    unresolved_fullwindow(Case::Ambiguous, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ambiguous_ordinary_fullwindow_preserves_previous() {
    unresolved_fullwindow(Case::Ambiguous, false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insufficient_fullwindow_refresh_preserves_previous() {
    unresolved_fullwindow(Case::Insufficient, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insufficient_ordinary_fullwindow_preserves_previous() {
    unresolved_fullwindow(Case::Insufficient, false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncated_pcm_no_match_adaptive_refresh_preserves_previous() {
    let s = review_scenario(Case::TruncatedNoMatch, "adaptive").await;
    let old = old_marker(&s);
    let job = refresh(&s).await;
    assert_eq!(job.status, "succeeded", "{job:?}");
    assert_preserved_intro_verified_outro(&s, &old);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncated_pcm_no_match_ordinary_adaptive_preserves_previous() {
    let s = review_scenario(Case::TruncatedNoMatch, "adaptive").await;
    let old = old_marker(&s);
    probe(&s).await;
    assert_ordinary_uncertain(&s);
    assert_old(&s, &old);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_complete_pcm_no_match_adaptive_refresh_retains_old_intro() {
    let s = review_scenario(Case::CompleteNoMatch, "adaptive").await;
    let old = old_marker(&s);
    let job = refresh(&s).await;
    assert_eq!(job.status, "succeeded", "{job:?}");
    let after = s
        .store
        .get_fixture_marker(s.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(
        (after.intro_start_ms, after.intro_end_ms),
        (old.intro_start_ms, old.intro_end_ms)
    );
    assert!(after.outro_start_ms.is_some());
    let outcome = latest_outcome(&s);
    assert_eq!(outcome["kinds"]["intro"]["status"], "uncertain");
    assert!(
        outcome["kinds"]["intro"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("sampling_limit"))
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_source_chapters_reanalyze_unchanged_fullwindow_fingerprints() {
    let mut s = scenario().await;
    probe(&s).await;
    let old = s
        .store
        .get_fixture_marker(s.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    let before = latest_outcome(&s);
    s.store
        .put_cached_chapters(
            &s.ledgers[0].file.file_id.to_string(),
            &[marker::ChapterMarker {
                start_ms: 20_000,
                end_ms: 95_000,
                title: Some("New source boundary".into()),
                marker_type: None,
                synthetic: false,
            }],
        )
        .unwrap();
    s.manager = Arc::new(
        ProbeManager::with_engines(
            s.store.clone(),
            Arc::new(FixedCandidate),
            Arc::new(FixedCandidate),
        )
        .unwrap(),
    );
    assert!(s.manager.try_start_workers());
    probe(&s).await;
    assert_ordinary_uncertain(&s);
    assert_old(&s, &old);
    assert_ne!(
        latest_outcome(&s)["available_reference_key"],
        before["available_reference_key"]
    );
}

struct NoRecapture;
impl marker::FingerprintCaptureEngine for NoRecapture {
    fn capture_window(
        &self,
        _: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        Err(marker::CaptureFailure {
            kind: marker::CaptureFailureKind::Io,
            message: "audio recapture forbidden".into(),
            metrics: Default::default(),
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_boundary_policy_reanalyzes_valid_captures_without_recapture() {
    let mut s = scenario().await;
    probe(&s).await;
    let before = latest_outcome(&s);
    // Frozen public identity at a55abe63; retaining it must not skip the new
    // evidence policy even when all captured words/chapters are unchanged.
    let legacy = "c2e3ad6a9e73a06c538a5d17e9b7dde3cb5a52061bf831286fbec9c0dbcc3403";
    let current = api::fingerprint_job::analysis_policy_key(&marker::SamplingPolicy::default());
    s.store
        .save_season_comparison_digest(&s.media_id.to_string(), 1, legacy, 1)
        .unwrap();
    s.manager = Arc::new(
        ProbeManager::with_engines(
            s.store.clone(),
            Arc::new(FixedCandidate),
            Arc::new(NoRecapture),
        )
        .unwrap(),
    );
    assert!(s.manager.try_start_workers());
    probe(&s).await;
    assert_eq!(s.fingerprint_job_status(1), "succeeded");
    let after = latest_outcome(&s);
    assert_eq!(after["analysis_policy_key"], current);
    assert_eq!(after["status"], "verified");
    assert_eq!(
        after["available_reference_key"],
        before["available_reference_key"]
    );
    assert!(
        s.store
            .get_fixture_marker(s.media_id, Some(1), Some(1))
            .unwrap()
            .is_some()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_known_pcm_origin_reanalyzes_cached_fingerprints() {
    let mut s = scenario().await;
    probe(&s).await;
    let old = s
        .store
        .get_fixture_marker(s.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    let before = latest_outcome(&s);
    // A corrected duration and observed tail window remain a valid capture for
    // the current request. Changing only the origin would contradict that window
    // and must instead recapture (covered in reuse_contract).
    for row in &s.ledgers {
        let key = row.file.file_id.to_string();
        let mut cached = s.store.get_fingerprint_cache(&key).unwrap().unwrap();
        cached.media_duration_ms = Some(2_405_000);
        let tail = cached.outro.as_mut().unwrap();
        tail.window = marker::SampleWindow::new(2_225_000, 2_405_000);
        tail.metrics.contiguous_pcm_start_ms = Some(2_225_000);
        tail.metrics.first_decoded_source_pts = Some(35_600_000);
        s.store.put_fingerprint_cache(&key, &cached).unwrap();
        let tracks = s.store.get_file_meta(&key).unwrap().unwrap();
        s.store
            .put_file_meta_versioned(
                &key,
                &tracks,
                Some(&row.file.version.source_version),
                cached.media_duration_ms,
            )
            .unwrap();
    }
    s.manager = Arc::new(
        ProbeManager::with_engines(
            s.store.clone(),
            Arc::new(FixedCandidate),
            Arc::new(NoRecapture),
        )
        .unwrap(),
    );
    assert!(s.manager.try_start_workers());
    probe(&s).await;
    assert_eq!(s.fingerprint_job_status(1), "succeeded");
    let after = s
        .store
        .get_fixture_marker(s.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(
        (after.intro_start_ms, after.intro_end_ms),
        (old.intro_start_ms, old.intro_end_ms)
    );
    assert_eq!(
        (after.outro_start_ms, after.outro_end_ms),
        (Some(2_255_000), Some(2_315_000))
    );
    assert_ne!(
        latest_outcome(&s)["available_reference_key"],
        before["available_reference_key"]
    );
}

#[path = "reuse_contract.rs"]
mod reuse_contract;

async fn absence_outside_capture_preserves_previous(mode: &str) {
    let s = review_scenario(Case::CompleteNoMatch, mode).await;
    let mut old = old_marker(&s);
    old.intro_start_ms = Some(300_000);
    old.intro_end_ms = Some(330_000);
    s.store.put_fixture_marker(&old).unwrap();
    let job = refresh(&s).await;
    assert_eq!(job.status, "succeeded", "{job:?}");
    assert_preserved_intro_verified_outro(&s, &old);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fullwindow_no_match_cannot_delete_a_marker_outside_capture() {
    absence_outside_capture_preserves_previous("full_window").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adaptive_no_match_cannot_delete_a_marker_outside_capture() {
    absence_outside_capture_preserves_previous("adaptive").await;
}

struct ChapterChangedDuringMatching {
    store: Arc<store::Store>,
    ledger_id: String,
    changed: std::sync::atomic::AtomicBool,
}
impl FingerprintEngine for ChapterChangedDuringMatching {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        min: f32,
        max: f32,
    ) -> Option<CommonSegment> {
        if !self.changed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            self.store
                .put_cached_chapters(
                    &self.ledger_id,
                    &[marker::ChapterMarker {
                        start_ms: 20_000,
                        end_ms: 95_000,
                        title: None,
                        marker_type: None,
                        synthetic: false,
                    }],
                )
                .unwrap();
        }
        FixedCandidate.find_common_segment(first, second, min, max)
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_chapters_reject_the_inflight_snapshot_before_current_review() {
    let mut s = scenario().await;
    s.manager.shutdown_workers().await.unwrap();
    let matcher = Arc::new(ChapterChangedDuringMatching {
        store: s.store.clone(),
        ledger_id: s.ledgers[0].file.file_id.to_string(),
        changed: std::sync::atomic::AtomicBool::new(false),
    });
    s.manager = Arc::new(
        ProbeManager::with_engines(s.store.clone(), matcher.clone(), Arc::new(FixedCandidate))
            .unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
    assert!(s.manager.enqueue(unit(&s.ledgers[0], false)));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !matcher.changed.load(std::sync::atomic::Ordering::SeqCst) {
            advance_analysis(&s);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        loop {
            if s.store
                .active_probe_unit_for_ledger(&matcher.ledger_id)
                .unwrap()
                .is_some_and(|unit| unit.status == "queued")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        s.store
            .get_fixture_marker(s.media_id, Some(1), Some(1))
            .unwrap()
            .is_none(),
        "an old snapshot cannot publish after its source chapters change"
    );
    finish_probe(&s).await;
    assert_ordinary_uncertain(&s);
    assert!(
        s.store
            .get_fixture_marker(s.media_id, Some(1), Some(1))
            .unwrap()
            .is_none()
    );
}
