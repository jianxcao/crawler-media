//! Step 4 TDD regression suite for remote known-stream seek probe reduction,
//! O(log N) SeasonMatchSession LRU eviction under high churn, and real
//! SourceCostSummary derivation from capture attempt metrics.

mod common;

use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use common::make_target_evidence;
use marker::adaptive::{
    AttemptCostObservation, EpisodeDescriptor, SamplingPolicy, SegmentKind, SourceCostSummary,
    TemplateContext, TemplateModel, TemplateReference, WindowDecision, plan_episode_window,
};
use marker::matcher::session::SeasonMatchSession;
use marker::{FingerprintEngine, PairFingerprintDecision, ProbeTarget, WorkControl};

#[test]
fn remote_known_stream_seek_uses_compact_probe_budget_while_unknown_keeps_10mb() {
    let remote = ProbeTarget::Remote("https://storage.example/series/s01e01.mkv".into());

    // Known stream interior/tail seek (start_ms > 0) uses 2s / 1MB probe budget instead of 10s / 10MB.
    let mut seek_cmd = Command::new("ffmpeg");
    seek_cmd.env("HTTP_PROXY", "http://127.0.0.1:7890");
    remote.apply_ffmpeg_input_known_stream_seek_ms(&mut seek_cmd, 1_320_000);
    let args = seek_cmd
        .get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    let analyze_idx = args
        .iter()
        .position(|a| a == "-analyzeduration")
        .expect("-analyzeduration present");
    let probesize_idx = args
        .iter()
        .position(|a| a == "-probesize")
        .expect("-probesize present");
    let ss_idx = args.iter().position(|a| a == "-ss").expect("-ss present");
    let proxy_idx = args
        .iter()
        .position(|a| a == "-http_proxy")
        .expect("-http_proxy present");

    assert_eq!(args[analyze_idx + 1], "2000000");
    assert_eq!(args[probesize_idx + 1], "1000000");
    assert_eq!(args[ss_idx + 1], "1320");
    assert_eq!(args[proxy_idx + 1], "");
    assert!(
        seek_cmd
            .get_envs()
            .any(|(k, v)| k == "HTTP_PROXY" && v.is_none()),
        "proxy env must still be stripped"
    );

    // Head capture (start_ms == 0) and generic unknown-stream apply_ffmpeg_input_with_seek_ms keep 10MB.
    let mut head_cmd = Command::new("ffmpeg");
    remote.apply_ffmpeg_input_known_stream_seek_ms(&mut head_cmd, 0);
    let head_args = head_cmd
        .get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let head_ps = head_args.iter().position(|a| a == "-probesize").unwrap();
    assert_eq!(head_args[head_ps + 1], "10000000");

    let mut generic_cmd = Command::new("ffmpeg");
    remote.apply_ffmpeg_input_with_seek_ms(&mut generic_cmd, 1_320_000);
    let generic_args = generic_cmd
        .get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let generic_ps = generic_args.iter().position(|a| a == "-probesize").unwrap();
    assert_eq!(generic_args[generic_ps + 1], "10000000");
}

struct FastNoMatchEngine;

impl FingerprintEngine for FastNoMatchEngine {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<marker::CommonSegment> {
        None
    }
    fn compare_fingerprints(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> PairFingerprintDecision {
        PairFingerprintDecision::NoMatch
    }
}

#[test]
fn season_match_session_sustained_eviction_churn_preserves_lru_and_scales_fast() {
    let capacity = 4_000usize;
    let session = Arc::new(SeasonMatchSession::new(
        Arc::new(FastNoMatchEngine),
        capacity,
        32 * 1024 * 1024,
    ));
    let anchor = make_target_evidence("anchor", 1, SegmentKind::Intro, 1, 0, 180_000);
    let peers = (0..12_000u32)
        .map(|i| {
            make_target_evidence(
                &format!("p-{i}"),
                i + 2,
                SegmentKind::Intro,
                i + 2,
                0,
                180_000,
            )
        })
        .collect::<Vec<_>>();

    // Fill cache to capacity (0..4_000)
    for peer in &peers[..capacity] {
        session
            .observe("pol-1".into())
            .measure(|e| e.compare_evidence(&anchor, peer, 15.0, 240.0));
    }
    // Touch the first 100 entries so they become most-recently-used!
    for peer in &peers[..100] {
        let (_, work) = session
            .observe("pol-1".into())
            .measure(|e| e.compare_evidence(&anchor, peer, 15.0, 240.0));
        assert_eq!(work.reused, 1);
    }

    let started = Instant::now();
    // Evict 3_900 untouched entries by inserting 3_900 new entries
    for peer in &peers[capacity..capacity + 3_900] {
        session
            .observe("pol-1".into())
            .measure(|e| e.compare_evidence(&anchor, peer, 15.0, 240.0));
    }
    // Additional 4_100 churn insertions
    for peer in &peers[capacity + 3_900..12_000] {
        // Re-touch peer[0] periodically so it stays hottest across all 8_000 evictions!
        let _ = session
            .observe("pol-1".into())
            .measure(|e| e.compare_evidence(&anchor, &peers[0], 15.0, 240.0));
        session
            .observe("pol-1".into())
            .measure(|e| e.compare_evidence(&anchor, peer, 15.0, 240.0));
    }
    let elapsed = started.elapsed();

    // peer[0] was continually refreshed -> must still be cached!
    let (_, hot_work) = session
        .observe("pol-1".into())
        .measure(|e| e.compare_evidence(&anchor, &peers[0], 15.0, 240.0));
    assert_eq!(
        hot_work.reused, 1,
        "frequently touched key must survive churn"
    );

    // peer[150] was never touched after initial fill -> must have been evicted!
    let (_, cold_work) = session
        .observe("pol-1".into())
        .measure(|e| e.compare_evidence(&anchor, &peers[150], 15.0, 240.0));
    assert_eq!(
        cold_work.computed, 1,
        "untouched oldest key must be evicted"
    );

    assert!(
        elapsed.as_millis() < 1_500,
        "8,000 evictions at capacity 4,000 took {:?} (expected O(log N) << 1.5s)",
        elapsed
    );
}

#[test]
fn source_cost_summary_derives_medians_and_disables_unstable_fast_path() {
    let observations = vec![
        AttemptCostObservation {
            phase: "full_window".into(),
            succeeded: true,
            window_duration_ms: 180_000,
            elapsed_ms: 9_000, // 20.0 sample_ms / wall_ms
            time_to_first_pcm_ms: Some(400),
        },
        AttemptCostObservation {
            phase: "fast_verify".into(),
            succeeded: true,
            window_duration_ms: 100_000,
            elapsed_ms: 4_000, // 25.0 sample_ms / wall_ms
            time_to_first_pcm_ms: Some(350),
        },
        AttemptCostObservation {
            phase: "fallback_full_window".into(),
            succeeded: true,
            window_duration_ms: 180_000,
            elapsed_ms: 10_000, // 18.0 sample_ms / wall_ms
            time_to_first_pcm_ms: Some(450),
        },
        AttemptCostObservation {
            phase: "fast_verify".into(),
            succeeded: false,
            window_duration_ms: 100_000,
            elapsed_ms: 2_000,
            time_to_first_pcm_ms: None,
        },
        AttemptCostObservation {
            phase: "fast_verify".into(),
            succeeded: true,
            window_duration_ms: 100_000,
            elapsed_ms: 5_000, // 20.0 sample_ms / wall_ms
            time_to_first_pcm_ms: Some(380),
        },
        AttemptCostObservation {
            phase: "fallback_full_window".into(),
            succeeded: true,
            window_duration_ms: 180_000,
            elapsed_ms: 9_000, // 20.0 sample_ms / wall_ms
            time_to_first_pcm_ms: Some(410),
        },
        AttemptCostObservation {
            phase: "fast_verify".into(),
            succeeded: true,
            window_duration_ms: 100_000,
            elapsed_ms: 5_000,
            time_to_first_pcm_ms: Some(390),
        },
        AttemptCostObservation {
            phase: "fast_verify".into(),
            succeeded: true,
            window_duration_ms: 100_000,
            elapsed_ms: 5_000,
            time_to_first_pcm_ms: Some(400),
        },
    ];

    let summary = SourceCostSummary::from_observations(&observations);
    assert_eq!(summary.measured_samples, 7);
    assert_eq!(summary.median_first_pcm_ms, Some(400));
    let rate = summary.sample_ms_per_wall_ms.expect("rate computed");
    assert!((19.9..=20.1).contains(&rate), "median rate got {rate}");
    assert_eq!(summary.recent_fast_attempts, 5);
    assert_eq!(summary.recent_fallbacks, 3);

    // Planner must disable fast path when >= 3 of last 5 fast attempts fell back or failed.
    let templates = TemplateContext {
        models: vec![TemplateModel {
            model_id: "intro-90".into(),
            version: 1,
            kind: SegmentKind::Intro,
            references: (1..=3)
                .map(|ep| TemplateReference {
                    sample_id: format!("s{ep}"),
                    ledger_id: format!("l{ep}"),
                    episode: ep,
                    source_version: "v1".into(),
                    match_interval_ms: (20_000, 110_000),
                    match_from_end_ms: None,
                })
                .collect(),
            expected_duration_ms: 90_000,
            is_stable: true,
        }],
        ..Default::default()
    };
    let ep = EpisodeDescriptor {
        ledger_id: "l9".into(),
        episode: 9,
        source_version: "v1".into(),
        duration_ms: 1_440_000,
    };
    let decision = plan_episode_window(
        &ep,
        SegmentKind::Intro,
        &templates,
        &SamplingPolicy::default(),
        &summary,
    );
    assert!(
        matches!(decision, WindowDecision::Full { ref reason, .. } if reason == "fast_path_disabled_unstable"),
        "expected fast_path_disabled_unstable, got {decision:?}"
    );
}
