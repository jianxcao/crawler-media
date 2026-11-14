use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use marker::{
    CaptureRequest, CapturedFingerprint, FINGERPRINT_ALGORITHM_VERSION, FingerprintCaptureEngine,
    SampleWindow,
};
use store::FingerprintCacheEntry;

use super::{
    capture_profile_key, fingerprint_cache_key_with_profile, media_source_version,
    profile_for_source, reusable_capture,
};

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
    engine: Arc<dyn FingerprintCaptureEngine>,
    control: &marker::WorkControl,
) -> Result<FingerprintCaptureOutcome, String> {
    capture_or_reuse_fingerprints_from_source(
        path,
        source_version,
        sample_duration_secs,
        media_duration_ms,
        cached,
        allow_cache_reuse,
        engine,
        control,
        None,
    )
    .await
}

pub async fn capture_or_reuse_fingerprints_from_source(
    path: &Path,
    source_version: &str,
    sample_duration_secs: u32,
    media_duration_ms: Option<i64>,
    cached: Option<FingerprintCacheEntry>,
    allow_cache_reuse: bool,
    engine: Arc<dyn FingerprintCaptureEngine>,
    control: &marker::WorkControl,
    source: Option<marker::CaptureSourceFacts>,
) -> Result<FingerprintCaptureOutcome, String> {
    if control.is_cancelled() {
        return Err("Library work cancelled".into());
    }
    let profile = profile_for_source(source.as_ref());
    let profile_key = capture_profile_key(&profile);
    let cache_key = fingerprint_cache_key_with_profile(
        source_version,
        sample_duration_secs,
        media_duration_ms,
        &profile,
    );
    let intro_window = SampleWindow::new(0, i64::from(sample_duration_secs) * 1000);
    let cached = cached.filter(|entry| {
        allow_cache_reuse
            && entry.cache_key == cache_key
            && entry.algorithm_version == FINGERPRINT_ALGORITHM_VERSION
            && entry.sample_duration_secs == sample_duration_secs
    });
    let (intro, intro_cache_hit, intro_elapsed_ms) = capture_intro(
        path,
        source_version,
        sample_duration_secs,
        cached.as_ref().filter(|entry| {
            reusable_capture(&entry.intro, &profile_key, &intro_window, source.as_ref())
        }),
        &engine,
        control,
        source.clone(),
    )
    .await?;

    let (outro, outro_cache_hit, outro_elapsed_ms, outro_error) = capture_outro(
        path,
        sample_duration_secs,
        media_duration_ms,
        cached.as_ref(),
        engine,
        control,
        source,
    )
    .await;

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
        intro_words = complete_cache.intro.words.len(),
        outro_words = complete_cache.outro.as_ref().map(|capture| capture.words.len()).unwrap_or_default(),
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

async fn capture_outro(
    path: &Path,
    sample_duration_secs: u32,
    media_duration_ms: Option<i64>,
    cached: Option<&FingerprintCacheEntry>,
    engine: Arc<dyn FingerprintCaptureEngine>,
    control: &marker::WorkControl,
    source: Option<marker::CaptureSourceFacts>,
) -> (Option<CapturedFingerprint>, bool, u64, Option<String>) {
    let profile_key = capture_profile_key(&profile_for_source(source.as_ref()));
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
        .filter(|outro| {
            reusable_capture(
                outro,
                &profile_key,
                &SampleWindow::new(
                    media_duration_ms.unwrap_or_default() - i64::from(sample_duration_secs) * 1000,
                    media_duration_ms.unwrap_or_default(),
                ),
                source.as_ref(),
            )
        })
    {
        (Some(outro.clone()), true, 0)
    } else {
        let duration = media_duration_ms.unwrap_or_default();
        let start_ms = duration - i64::from(sample_duration_secs) * 1000;
        let started = Instant::now();
        tracing::info!(
            path = %path.display(),
            sample_start_ms = start_ms,
            sample_duration_secs,
            total_duration_ms = duration,
            "【声纹】片尾声纹缓存未命中，开始采集"
        );
        match extract(
            engine,
            path.to_path_buf(),
            start_ms,
            sample_duration_secs,
            control,
            source.clone(),
        )
        .await
        {
            Ok(outro) if !outro.words.is_empty() => {
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

    (outro, outro_cache_hit, outro_elapsed_ms, outro_error)
}

pub fn current_source_version(path: &Path) -> String {
    media_source_version(path)
}

/// Observe one source outside the Store lock for a FileVersion CAS: stat facts
/// come from `library::observe_source`, the version string is the single
/// production encoding [`current_source_version`] (Probe, fingerprints and
/// subtitles compare against it; `.strm` includes its content digest).
pub fn observe_source(path: &Path) -> Option<library::SourceObservation> {
    library::observe_source(path).ok()
}

async fn extract(
    engine: Arc<dyn FingerprintCaptureEngine>,
    path: PathBuf,
    start_ms: i64,
    duration_secs: u32,
    control: &marker::WorkControl,
    source: Option<marker::CaptureSourceFacts>,
) -> Result<CapturedFingerprint, String> {
    let control = control.clone();
    tokio::task::spawn_blocking(move || {
        let request = CaptureRequest {
            path,
            window: SampleWindow::new(start_ms, start_ms + i64::from(duration_secs) * 1000),
            audio_stream_index: source.as_ref().map(|s| s.audio_stream_index),
            process_deadline_ms: 120_000,
        };
        engine
            .capture_window_after_metadata_controlled(&request, source.as_ref(), &control)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("fingerprint worker failed: {error}"))?
}

async fn capture_intro(
    path: &Path,
    source_version: &str,
    sample_duration_secs: u32,
    cached: Option<&FingerprintCacheEntry>,
    engine: &Arc<dyn FingerprintCaptureEngine>,
    control: &marker::WorkControl,
    source: Option<marker::CaptureSourceFacts>,
) -> Result<(CapturedFingerprint, bool, u64), String> {
    match cached {
        Some(entry) => Ok((entry.intro.clone(), true, 0)),
        None => {
            let started = Instant::now();
            tracing::info!(
                path = %path.display(),
                sample_start_secs = 0,
                sample_duration_secs,
                source_version,
                "【声纹】片头声纹缓存未命中，开始采集"
            );
            let intro = extract(
                engine.clone(),
                path.to_path_buf(),
                0,
                sample_duration_secs,
                control,
                source,
            )
            .await?;
            if intro.words.is_empty() {
                return Err("empty intro fingerprint".into());
            }
            Ok((intro, false, started.elapsed().as_millis() as u64))
        }
    }
}
