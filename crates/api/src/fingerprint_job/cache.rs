use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use marker::{FINGERPRINT_ALGORITHM_VERSION, FingerprintEngine, extract_audio_fingerprint_at_with};
use store::FingerprintCacheEntry;

use super::{fingerprint_cache_key, media_source_version};

#[derive(Clone, Debug)]
pub struct FingerprintCaptureOutcome {
    pub cache: FingerprintCacheEntry,
    pub intro_cache_hit: bool,
    pub outro_cache_hit: bool,
    pub intro_elapsed_ms: u64,
    pub outro_elapsed_ms: u64,
    pub outro_error: Option<String>,
}

/// Capture only missing or stale samples. This is the replaceable media-read seam
/// shared by background jobs and deterministic performance tests.
pub async fn capture_or_reuse_fingerprints(
    path: &Path,
    source_version: &str,
    sample_duration_secs: u32,
    media_duration_ms: Option<i64>,
    cached: Option<FingerprintCacheEntry>,
    allow_cache_reuse: bool,
    engine: Arc<dyn FingerprintEngine>,
) -> Result<FingerprintCaptureOutcome, String> {
    let cache_key = fingerprint_cache_key(source_version, sample_duration_secs, media_duration_ms);
    let cached = cached.filter(|entry| {
        allow_cache_reuse
            && entry.cache_key == cache_key
            && entry.algorithm_version == FINGERPRINT_ALGORITHM_VERSION
            && entry.sample_duration_secs == sample_duration_secs
            && !entry.intro.is_empty()
    });
    let (intro, intro_cache_hit, intro_elapsed_ms) = match cached.as_ref() {
        Some(entry) => (entry.intro.clone(), true, 0),
        None => {
            let started = Instant::now();
            tracing::info!(
                path = %path.display(),
                sample_start_secs = 0,
                sample_duration_secs,
                source_version,
                "【声纹】片头声纹缓存未命中，开始采集"
            );
            let intro =
                extract(engine.clone(), path.to_path_buf(), 0, sample_duration_secs).await?;
            if intro.is_empty() {
                return Err("empty intro fingerprint".into());
            }
            (intro, false, started.elapsed().as_millis() as u64)
        }
    };

    let mut outro_error = None;
    let outro_expected = media_duration_ms
        .filter(|duration| *duration > 0)
        .is_some_and(|duration| {
            duration / 1000 > i64::from(sample_duration_secs.saturating_add(30))
        });
    let (outro, outro_cache_hit, outro_elapsed_ms) = if !outro_expected {
        if media_duration_ms.is_none_or(|duration| duration <= 0) {
            outro_error = Some("container duration is missing".to_string());
        }
        (None, false, 0)
    } else if let Some(outro) = cached
        .as_ref()
        .and_then(|entry| entry.outro.as_ref())
        .filter(|outro| !outro.is_empty())
    {
        (Some(outro.clone()), true, 0)
    } else {
        let duration = media_duration_ms.unwrap_or_default();
        let start_secs = (duration / 1000) as u32 - sample_duration_secs;
        let started = Instant::now();
        tracing::info!(
            path = %path.display(),
            sample_start_secs = start_secs,
            sample_duration_secs,
            total_duration_ms = duration,
            source_version,
            "【声纹】片尾声纹缓存未命中，开始采集"
        );
        match extract(engine, path.to_path_buf(), start_secs, sample_duration_secs).await {
            Ok(outro) if !outro.is_empty() => {
                (Some(outro), false, started.elapsed().as_millis() as u64)
            }
            Ok(_) => {
                outro_error = Some("empty outro fingerprint".into());
                (None, false, started.elapsed().as_millis() as u64)
            }
            Err(error) => {
                outro_error = Some(error);
                (None, false, started.elapsed().as_millis() as u64)
            }
        }
    };

    let complete_cache = FingerprintCacheEntry {
        cache_key,
        algorithm_version: FINGERPRINT_ALGORITHM_VERSION,
        sample_duration_secs,
        media_duration_ms,
        intro,
        outro,
    };
    tracing::info!(
        path = %path.display(),
        intro_cache_hit,
        outro_cache_hit,
        intro_words = complete_cache.intro.len(),
        outro_words = complete_cache.outro.as_ref().map(Vec::len).unwrap_or_default(),
        intro_elapsed_ms,
        outro_elapsed_ms,
        complete = outro_error.is_none(),
        error = ?outro_error,
        "【声纹】单集指纹采集与缓存复用汇总"
    );
    Ok(FingerprintCaptureOutcome {
        cache: complete_cache,
        intro_cache_hit,
        outro_cache_hit,
        intro_elapsed_ms,
        outro_elapsed_ms,
        outro_error,
    })
}

pub fn current_source_version(path: &Path) -> String {
    media_source_version(path)
}

async fn extract(
    engine: Arc<dyn FingerprintEngine>,
    path: PathBuf,
    start_secs: u32,
    duration_secs: u32,
) -> Result<Vec<u32>, String> {
    tokio::task::spawn_blocking(move || {
        extract_audio_fingerprint_at_with(engine.as_ref(), &path, start_secs, duration_secs)
    })
    .await
    .map_err(|error| format!("fingerprint worker failed: {error}"))?
}
