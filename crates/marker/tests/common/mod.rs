//! Shared fixtures for the marker adaptive-verification integration tests.
//!
//! This module is included by several test binaries, so items unused by one of them are
//! expected and allowed.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;

use marker::{
    CaptureMetrics, CapturedFingerprint, CommonSegment, EpisodeEvidence, FingerprintEngine,
    SampleWindow, SegmentKind, TemplateModel, TemplateReference,
};

#[derive(Clone, Default)]
pub struct MockVerificationEngine {
    // Return segments based on key
    pub returns: HashMap<(u32, u32), Vec<CommonSegment>>,
}

impl FingerprintEngine for MockVerificationEngine {
    fn extract_at(
        &self,
        _path: &Path,
        _start_secs: u32,
        _duration_secs: u32,
    ) -> Result<Vec<u32>, String> {
        Ok(Vec::new())
    }

    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        min_duration_secs: f32,
        max_duration_secs: f32,
    ) -> Option<CommonSegment> {
        self.find_common_segments(first, second, min_duration_secs, max_duration_secs)
            .into_iter()
            .next()
    }

    fn find_common_segments(
        &self,
        first: &[u32],
        second: &[u32],
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Vec<CommonSegment> {
        let tag1 = first.first().copied().unwrap_or(0);
        let tag2 = second.first().copied().unwrap_or(0);
        if let Some(res) = self.returns.get(&(tag1, tag2)) {
            return res.clone();
        }
        if let Some(res) = self.returns.get(&(tag2, tag1)) {
            // Inverted
            return res
                .iter()
                .map(|s| CommonSegment {
                    start1_sec: s.start2_sec,
                    end1_sec: s.end2_sec,
                    start2_sec: s.start1_sec,
                    end2_sec: s.end1_sec,
                    duration_sec: s.duration_sec,
                    score: s.score,
                })
                .collect();
        }
        Vec::new()
    }
}

pub fn make_target_evidence(
    sample_id: &str,
    episode: u32,
    kind: SegmentKind,
    tag: u32,
    window_start_ms: i64,
    window_end_ms: i64,
) -> EpisodeEvidence {
    let len = 500;
    let mut words = Vec::with_capacity(len);
    for i in 0..len {
        words.push(tag + i as u32);
    }
    EpisodeEvidence {
        sample_id: sample_id.to_string(),
        ledger_id: format!("ledger-{}", episode),
        episode,
        source_version: "v1".to_string(),
        capture_profile_key: "default".to_string(),
        kind,
        capture: CapturedFingerprint {
            window: SampleWindow {
                start_ms: window_start_ms,
                end_ms: window_end_ms,
            },
            words,
            pcm_duration_ms: Some(window_end_ms - window_start_ms),
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
        duration_ms: Some(1_200_000),
    }
}

/// Build an intro template reference whose reference interval is `match_interval_ms`.
pub fn template_reference(
    sample_id: &str,
    episode: u32,
    match_interval_ms: (i64, i64),
) -> TemplateReference {
    TemplateReference {
        sample_id: sample_id.to_string(),
        ledger_id: format!("ledger-{}", episode),
        episode,
        source_version: "v1".to_string(),
        match_interval_ms,
        match_from_end_ms: None,
    }
}

/// Build an intro template model from already built references.
pub fn intro_template_model(
    model_id: &str,
    references: Vec<TemplateReference>,
    expected_duration_ms: i64,
) -> TemplateModel {
    TemplateModel {
        model_id: model_id.to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        references,
        expected_duration_ms,
        is_stable: true,
    }
}

/// Build a common segment with target coordinates and reference coordinates in seconds.
pub fn common_segment(target: (f32, f32), reference: (f32, f32), score: f64) -> CommonSegment {
    CommonSegment {
        start1_sec: target.0,
        end1_sec: target.1,
        start2_sec: reference.0,
        end2_sec: reference.1,
        duration_sec: target.1 - target.0,
        score,
    }
}
