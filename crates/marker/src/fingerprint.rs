mod chromaprint;
mod segments;

use std::path::Path;
use std::time::Instant;

use crate::target::ProbeTarget;
use crate::types::CommonSegment;

pub type AudioFingerprint = Vec<u32>;

pub const MIN_MATCH_DURATION_SECS: f32 = 15.0;
pub const MAX_MATCH_DURATION_SECS: f32 = 240.0;
const MAX_REMOTE_TRUNCATION_ATTEMPTS: u8 = 2;

/// Replaceable boundary for audio fingerprint extraction and matching.
pub trait FingerprintEngine: Send + Sync {
    fn extract_at(
        &self,
        path: &Path,
        start_secs: u32,
        duration_secs: u32,
    ) -> Result<AudioFingerprint, String>;

    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        min_duration_secs: f32,
        max_duration_secs: f32,
    ) -> Option<CommonSegment>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ChromaprintEngine;

impl FingerprintEngine for ChromaprintEngine {
    fn extract_at(
        &self,
        path: &Path,
        start_secs: u32,
        duration_secs: u32,
    ) -> Result<AudioFingerprint, String> {
        chromaprint::extract(path, start_secs, duration_secs)
    }

    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        min_duration_secs: f32,
        max_duration_secs: f32,
    ) -> Option<CommonSegment> {
        segments::find_common_segment(first, second, min_duration_secs, max_duration_secs)
    }
}

pub fn extract_audio_fingerprint(path: &Path, max_duration_secs: u32) -> Result<Vec<u32>, String> {
    extract_audio_fingerprint_at(path, 0, max_duration_secs)
}

pub fn extract_audio_fingerprint_at(
    path: &Path,
    start_secs: u32,
    duration_secs: u32,
) -> Result<Vec<u32>, String> {
    extract_audio_fingerprint_at_with(&ChromaprintEngine, path, start_secs, duration_secs)
}

pub fn extract_audio_fingerprint_at_with(
    engine: &dyn FingerprintEngine,
    path: &Path,
    start_secs: u32,
    duration_secs: u32,
) -> Result<AudioFingerprint, String> {
    let source = match ProbeTarget::from_path(path) {
        ProbeTarget::Local(_) => "local",
        ProbeTarget::Remote(_) => "remote",
    };
    let started = Instant::now();
    tracing::info!(
        path = %path.display(), source, start_secs, duration_secs,
        "【声纹】音频指纹生成开始"
    );

    let mut attempt = 1;
    let result = loop {
        let result = engine.extract_at(path, start_secs, duration_secs);
        let retryable_truncation = matches!(
            &result,
            Err(error) if error.contains("File ended prematurely")
        );
        if source != "remote" || !retryable_truncation || attempt >= MAX_REMOTE_TRUNCATION_ATTEMPTS
        {
            break result;
        }
        if let Err(error) = &result {
            tracing::warn!(
                path = %path.display(),
                source,
                attempt,
                next_attempt = attempt + 1,
                max_attempts = MAX_REMOTE_TRUNCATION_ATTEMPTS,
                error = %error,
                "【声纹】远端音频样本读取不完整，准备重试"
            );
        }
        attempt += 1;
        std::thread::sleep(std::time::Duration::from_millis(250));
    };
    let elapsed_ms = started.elapsed().as_millis();
    match &result {
        Ok(fingerprint) => tracing::info!(
            path = %path.display(), source, start_secs, duration_secs,
            fingerprint_items = fingerprint.len(), elapsed_ms,
            "【声纹】音频指纹生成成功"
        ),
        Err(error) => tracing::error!(
            path = %path.display(), source, start_secs, duration_secs,
            elapsed_ms, error = %error,
            "【声纹】音频指纹生成失败"
        ),
    }
    result
}

pub fn find_common_segment(
    first: &[u32],
    second: &[u32],
    min_duration_secs: f32,
    max_duration_secs: f32,
) -> Option<CommonSegment> {
    find_common_segment_with(
        &ChromaprintEngine,
        first,
        second,
        min_duration_secs,
        max_duration_secs,
    )
}

pub fn find_common_segment_with(
    engine: &dyn FingerprintEngine,
    first: &[u32],
    second: &[u32],
    min_duration_secs: f32,
    max_duration_secs: f32,
) -> Option<CommonSegment> {
    engine.find_common_segment(first, second, min_duration_secs, max_duration_secs)
}
