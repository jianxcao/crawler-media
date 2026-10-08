use std::path::Path;

use marker::{
    CaptureMetrics, CapturedFingerprint, CommonSegment, EpisodeDescriptor, EpisodeEvidence,
    FingerprintEngine, SampleWindow, SamplingPolicy, SegmentKind,
    build_season_models, select_seed_episodes,
};

#[derive(Clone, Default)]
struct MockEngine {}

impl FingerprintEngine for MockEngine {
    fn extract_at(&self, _path: &Path, _start_secs: u32, _duration_secs: u32) -> Result<Vec<u32>, String> {
        Ok(Vec::new())
    }

    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        min_duration_secs: f32,
        max_duration_secs: f32,
    ) -> Option<CommonSegment> {
        self.find_common_segments(first, second, min_duration_secs, max_duration_secs).into_iter().next()
    }

    fn find_common_segments(
        &self,
        first: &[u32],
        second: &[u32],
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Vec<marker::CommonSegment> {
        if first.is_empty() || second.is_empty() {
            return Vec::new();
        }
        // If both are dummy "intro" tokens
        if first.first() == Some(&100) && second.first() == Some(&100) {
            return vec![marker::CommonSegment {
                start1_sec: 10.0,
                end1_sec: 100.0,
                start2_sec: 10.0,
                end2_sec: 100.0,
                duration_sec: 90.0,
                score: 1.0,
            }];
        }
        // If variant A
        if first.first() == Some(&200) && second.first() == Some(&200) {
            return vec![marker::CommonSegment {
                start1_sec: 0.0,
                end1_sec: 60.0,
                start2_sec: 0.0,
                end2_sec: 60.0,
                duration_sec: 60.0,
                score: 1.0,
            }];
        }
        // If variant B
        if first.first() == Some(&300) && second.first() == Some(&300) {
            return vec![marker::CommonSegment {
                start1_sec: 0.0,
                end1_sec: 90.0,
                start2_sec: 0.0,
                end2_sec: 90.0,
                duration_sec: 90.0,
                score: 1.0,
            }];
        }
        Vec::new()
    }
}

fn make_evidence(
    sample_id: &str,
    ledger_id: &str,
    episode: u32,
    kind: SegmentKind,
    tag: u32,
    len: usize,
) -> EpisodeEvidence {
    // Generate distinct words starting with tag so is_constant_or_silent is false
    let mut words = Vec::with_capacity(len);
    for i in 0..len {
        words.push(tag + i as u32);
    }
    EpisodeEvidence {
        sample_id: sample_id.to_string(),
        ledger_id: ledger_id.to_string(),
        episode,
        source_version: "v1".to_string(),
        capture_profile_key: "default".to_string(),
        kind,
        capture: CapturedFingerprint {
            window: SampleWindow {
                start_ms: 0,
                end_ms: 180_000,
            },
            words,
            pcm_duration_ms: Some(180_000),
            metrics: CaptureMetrics {
                elapsed_ms: 100,
                time_to_first_pcm_ms: Some(50),
                pcm_read_wait_us: 10,
                chromaprint_consume_us: 10,
                pcm_bytes: 5000,
                input_bytes: None,
                input_bytes_source: None,
                measurement_complete: true,
                ffmpeg_exit_code: Some(0),
                command_setup_ms: None,
                ffmpeg_spawn_ms: None,
                pcm_stream_elapsed_ms: None,
                chromaprint_finish_ms: None,
                ffmpeg_wait_ms: None,
                stderr_collect_ms: None,
                sample_count: None,
                ffmpeg_user_cpu_ms: None,
                ffmpeg_system_cpu_ms: None,
                ffmpeg_real_ms: None,
                ffmpeg_maxrss_kb: None,
                ffmpeg_stderr_bytes: None,
                ffmpeg_stderr_tail: None,
            },
        },
        duration_ms: Some(180_000),
    }
}

#[test]
fn select_seed_episodes_for_eight_episodes_selects_e02_e04_e07() {
    let episodes: Vec<EpisodeDescriptor> = (1..=8)
        .map(|ep| EpisodeDescriptor {
            ledger_id: format!("ledger-e{:02}", ep),
            episode: ep,
            source_version: "v1".to_string(),
            duration_ms: 1_200_000,
        })
        .collect();

    let policy = SamplingPolicy::default();
    let selected = select_seed_episodes(&episodes, &policy);
    assert_eq!(selected, vec!["ledger-e02", "ledger-e04", "ledger-e07"]);
}

#[test]
fn same_episode_versions_count_once_as_support() {
    let episodes = vec![
        EpisodeDescriptor {
            ledger_id: "ledger-e01".to_string(),
            episode: 1,
            source_version: "v1".to_string(),
            duration_ms: 1_200_000,
        },
        EpisodeDescriptor {
            ledger_id: "ledger-e02-1080p".to_string(),
            episode: 2,
            source_version: "v1".to_string(),
            duration_ms: 1_200_000,
        },
        EpisodeDescriptor {
            ledger_id: "ledger-e02-4k".to_string(),
            episode: 2,
            source_version: "v1".to_string(),
            duration_ms: 1_200_000,
        },
        EpisodeDescriptor {
            ledger_id: "ledger-e03".to_string(),
            episode: 3,
            source_version: "v1".to_string(),
            duration_ms: 1_200_000,
        },
    ];
    let policy = SamplingPolicy::default();
    let selected = select_seed_episodes(&episodes, &policy);
    assert_eq!(selected.len(), 3);
    let has_1080 = selected.contains(&"ledger-e02-1080p".to_string());
    let has_4k = selected.contains(&"ledger-e02-4k".to_string());
    assert!(!(has_1080 && has_4k), "Same episode should not be selected twice as seed");
}

#[test]
fn multiple_credit_variants_remain_separate() {
    let engine = MockEngine::default();
    let policy = SamplingPolicy::default();

    let ev1 = make_evidence("s1", "l1", 1, SegmentKind::Outro, 200, 500);
    let ev2 = make_evidence("s2", "l2", 2, SegmentKind::Outro, 200, 500);
    let ev3 = make_evidence("s3", "l3", 3, SegmentKind::Outro, 300, 700);
    let ev4 = make_evidence("s4", "l4", 4, SegmentKind::Outro, 300, 700);

    let models = build_season_models(&engine, &[ev1, ev2, ev3, ev4], &policy);
    assert_eq!(models.len(), 2, "Should create two separate models for two outro variants");
}

#[test]
fn constant_fingerprints_do_not_form_trusted_templates() {
    let engine = MockEngine::default();
    let policy = SamplingPolicy::default();

    // Constant fingerprint words (e.g., all 0s or all repeating constant numbers)
    let ev1 = make_evidence("s1", "l1", 1, SegmentKind::Intro, 0, 500);
    let ev2 = make_evidence("s2", "l2", 2, SegmentKind::Intro, 0, 500);
    let ev3 = make_evidence("s3", "l3", 3, SegmentKind::Intro, 0, 500);

    let models = build_season_models(&engine, &[ev1, ev2, ev3], &policy);
    assert!(models.is_empty(), "Constant or silent fingerprints must not form template models");
}
