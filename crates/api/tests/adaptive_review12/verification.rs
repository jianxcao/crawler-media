use super::*;

#[test]
fn control_raw_uniform_pipeline_succeeds() {
    let f = setup("uniform", &[1, 2, 3, 4]);
    let rep = raw_season(&f).unwrap();
    assert_eq!(rep.markers.len(), 4);
}

#[test]
fn no_match_for_both_segments_clears_only_the_matching_file() {
    let f = setup("version-missing", &[1, 2, 3, 1]);
    let absent = &f.units[3].row;
    f.store
        .put_fixture_marker(&store::StoredFileMarker {
            file_id: absent.file.file_id,
            version: absent.file.version.clone(),
            unit: absent.units[0],
            intro_start_ms: Some(5_000),
            intro_end_ms: Some(65_000),
            outro_start_ms: Some(900_000),
            outro_end_ms: Some(960_000),
            source: "old".into(),
            locked: false,
            updated_at: 0,
        })
        .unwrap();
    let replacement = raw_season(&f).unwrap();
    let chapters = replacement.chapter_updates.clone();
    let members = store::observe_sources(
        &f.store
            .source_snapshots(
                &f.units
                    .iter()
                    .map(|unit| unit.row.file.file_id)
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
    );
    f.store
        .complete_marker_refresh_checked("review9-job", &[replacement], &members)
        .unwrap();
    assert!(
        f.store
            .get_file_marker(absent.file.file_id, absent.units[0])
            .unwrap()
            .is_none()
    );
    let retained = &f.units[0].row;
    let marker = f
        .store
        .get_file_marker(retained.file.file_id, retained.units[0])
        .unwrap()
        .unwrap();
    assert_eq!(
        (marker.intro_start_ms, marker.intro_end_ms),
        (Some(10_000), Some(70_000))
    );
    assert_eq!(
        (marker.outro_start_ms, marker.outro_end_ms),
        (Some(830_000), Some(890_000))
    );
    for (file_id, expected_markers) in [(absent.file.file_id, 0), (retained.file.file_id, 2)] {
        let output = &chapters
            .iter()
            .find(|(id, _)| id == &file_id.to_string())
            .unwrap()
            .1;
        assert_eq!(
            output
                .iter()
                .filter(|chapter| chapter.marker_type.is_some())
                .count(),
            expected_markers
        );
    }
    assert_eq!(
        f.store
            .get_probe_job("review9-job")
            .unwrap()
            .unwrap()
            .status,
        "succeeded"
    );
}

struct ScoredEngine;
impl FingerprintEngine for ScoredEngine {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _control: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(
        &self,
        _: &[u32],
        reference: &[u32],
        _: f32,
        _: f32,
    ) -> Option<CommonSegment> {
        let (start, score) = match reference[0] {
            1 => (10., 0.),
            2 => (10., 4.),
            _ => (100., 1.),
        };
        Some(CommonSegment {
            start1_sec: start,
            end1_sec: start + 60.,
            start2_sec: 10.,
            end2_sec: 70.,
            duration_sec: 60.,
            score,
        })
    }
}
fn evidence(sample_id: String, episode: u32, tag: u32) -> marker::adaptive::EpisodeEvidence {
    marker::adaptive::EpisodeEvidence {
        sample_id: sample_id.clone(),
        ledger_id: sample_id,
        episode,
        source_version: "src".into(),
        capture_profile_key: "profile".into(),
        kind: marker::SegmentKind::Intro,
        duration_ms: Some(1_000_000),
        capture: CapturedFingerprint {
            window: marker::fingerprint::SampleWindow {
                start_ms: 0,
                end_ms: 180_000,
            },
            words: vec![tag, tag + 1, tag + 2],
            pcm_duration_ms: Some(180_000),
            metrics: CaptureMetrics::default(),
        },
    }
}
fn scored_verdict(duplicates: usize) -> marker::adaptive::VerificationOutcome {
    use marker::adaptive::{SamplingPolicy, TemplateContext, TemplateModel, TemplateReference};
    let mut samples = vec![
        evidence("r1".into(), 1, 1),
        evidence("r2".into(), 2, 2),
        evidence("r3".into(), 3, 3),
        evidence("r4".into(), 4, 4),
    ];
    for i in 0..duplicates {
        samples.push(evidence(format!("extra-{i}"), 1, 1));
    }
    let references = samples
        .iter()
        .map(|e| TemplateReference {
            sample_id: e.sample_id.clone(),
            ledger_id: e.ledger_id.clone(),
            episode: e.episode,
            source_version: e.source_version.clone(),
            match_interval_ms: (10_000, 70_000),
            match_from_end_ms: None,
        })
        .collect();
    let ctx = TemplateContext {
        models: vec![TemplateModel {
            model_id: "template".into(),
            version: 1,
            kind: marker::SegmentKind::Intro,
            references,
            expected_duration_ms: 60_000,
            is_stable: true,
        }],
        references: samples
            .into_iter()
            .map(|e| (e.sample_id.clone(), e))
            .collect(),
    };
    marker::adaptive::verify_template_window(
        &ScoredEngine,
        &evidence("target".into(), 9, 9),
        &ctx,
        &SamplingPolicy::default(),
    )
}
#[test]
fn duplicating_one_reference_must_not_reweight_cluster_scores() {
    use marker::adaptive::VerificationOutcome;
    let before = scored_verdict(0);
    let after = scored_verdict(5);
    println!("score-weight control={before:?}; duplicated E1={after:?}");
    let (VerificationOutcome::Verified(a), VerificationOutcome::Verified(b)) = (before, after)
    else {
        panic!("expected verified");
    };
    assert_eq!((a.start_ms, a.end_ms), (100_000, 160_000));
    assert_eq!(
        (b.start_ms, b.end_ms),
        (a.start_ms, a.end_ms),
        "one episode's identical copies must not change independent-consensus winner"
    );
}
