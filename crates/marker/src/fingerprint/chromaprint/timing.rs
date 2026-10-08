use std::path::Path;
use std::time::{Duration, Instant};

use super::diagnostics::{FfmpegBenchmark, sanitized_excerpt};

#[derive(Default)]
pub(super) struct ExtractionTimings {
    pub(super) command_setup_ms: u128,
    pub(super) ffmpeg_spawn_ms: u128,
    pub(super) time_to_first_pcm_ms: Option<u128>,
    pub(super) pcm_read_wait: Duration,
    pub(super) chromaprint_consume: Duration,
    pub(super) pcm_stream_elapsed_ms: u128,
    pub(super) chromaprint_finish_ms: u128,
    pub(super) ffmpeg_wait_ms: u128,
    pub(super) stderr_collect_ms: u128,
    pub(super) pcm_bytes: u64,
    pub(super) sample_count: u64,
    pub(super) ffmpeg_log_level: String,
    pub(super) ffmpeg_exit_code: Option<i32>,
    pub(super) ffmpeg_stderr_tail: String,
    pub(super) ffmpeg_stderr_bytes: u64,
    pub(super) ffmpeg_stderr_truncated: bool,
    pub(super) ffmpeg_benchmark: Option<FfmpegBenchmark>,
}

pub(super) fn log_extraction_timings(
    path: &Path,
    source: &str,
    start_secs: u32,
    duration_secs: u32,
    started: Instant,
    timings: &ExtractionTimings,
    result: &Result<Vec<u32>, String>,
) {
    match result {
        Ok(fingerprint) => tracing::info!(
            path = %path.display(), source, start_secs, duration_secs,
            command_setup_ms = timings.command_setup_ms,
            ffmpeg_spawn_ms = timings.ffmpeg_spawn_ms,
            time_to_first_pcm_ms = ?timings.time_to_first_pcm_ms,
            pcm_read_wait_us = timings.pcm_read_wait.as_micros(),
            chromaprint_consume_us = timings.chromaprint_consume.as_micros(),
            pcm_stream_elapsed_ms = timings.pcm_stream_elapsed_ms,
            chromaprint_finish_ms = timings.chromaprint_finish_ms,
            ffmpeg_wait_ms = timings.ffmpeg_wait_ms,
            stderr_collect_ms = timings.stderr_collect_ms,
            ffmpeg_log_level = %timings.ffmpeg_log_level,
            ffmpeg_exit_code = ?timings.ffmpeg_exit_code,
            ffmpeg_stderr_bytes = timings.ffmpeg_stderr_bytes,
            ffmpeg_stderr_truncated = timings.ffmpeg_stderr_truncated,
            ffmpeg_user_cpu_ms = ?timings.ffmpeg_benchmark.as_ref().and_then(|bench| bench.user_cpu_ms),
            ffmpeg_system_cpu_ms = ?timings.ffmpeg_benchmark.as_ref().and_then(|bench| bench.system_cpu_ms),
            ffmpeg_real_ms = ?timings.ffmpeg_benchmark.as_ref().and_then(|bench| bench.real_ms),
            ffmpeg_maxrss_kb = ?timings.ffmpeg_benchmark.as_ref().and_then(|bench| bench.max_rss_kb),
            pcm_bytes = timings.pcm_bytes,
            sample_count = timings.sample_count,
            fingerprint_items = fingerprint.len(),
            total_elapsed_ms = started.elapsed().as_millis(),
            "【声纹】FFmpeg 与 Chromaprint 阶段耗时拆分"
        ),
        Err(error) => tracing::error!(
            path = %path.display(), source, start_secs, duration_secs, %error,
            command_setup_ms = timings.command_setup_ms,
            ffmpeg_spawn_ms = timings.ffmpeg_spawn_ms,
            time_to_first_pcm_ms = ?timings.time_to_first_pcm_ms,
            pcm_read_wait_us = timings.pcm_read_wait.as_micros(),
            chromaprint_consume_us = timings.chromaprint_consume.as_micros(),
            pcm_stream_elapsed_ms = timings.pcm_stream_elapsed_ms,
            chromaprint_finish_ms = timings.chromaprint_finish_ms,
            ffmpeg_wait_ms = timings.ffmpeg_wait_ms,
            stderr_collect_ms = timings.stderr_collect_ms,
            ffmpeg_log_level = %timings.ffmpeg_log_level,
            ffmpeg_exit_code = ?timings.ffmpeg_exit_code,
            ffmpeg_stderr_bytes = timings.ffmpeg_stderr_bytes,
            ffmpeg_stderr_truncated = timings.ffmpeg_stderr_truncated,
            ffmpeg_stderr_excerpt = %sanitized_excerpt(&timings.ffmpeg_stderr_tail),
            ffmpeg_user_cpu_ms = ?timings.ffmpeg_benchmark.as_ref().and_then(|bench| bench.user_cpu_ms),
            ffmpeg_system_cpu_ms = ?timings.ffmpeg_benchmark.as_ref().and_then(|bench| bench.system_cpu_ms),
            ffmpeg_real_ms = ?timings.ffmpeg_benchmark.as_ref().and_then(|bench| bench.real_ms),
            ffmpeg_maxrss_kb = ?timings.ffmpeg_benchmark.as_ref().and_then(|bench| bench.max_rss_kb),
            pcm_bytes = timings.pcm_bytes,
            sample_count = timings.sample_count,
            total_elapsed_ms = started.elapsed().as_millis(),
            "【声纹】FFmpeg 与 Chromaprint 阶段失败及耗时拆分"
        ),
    }

    if result.is_ok() && timings.pcm_stream_elapsed_ms >= 10_000 {
        tracing::debug!(
            path = %path.display(), source, start_secs, duration_secs,
            ffmpeg_log_level = %timings.ffmpeg_log_level,
            ffmpeg_exit_code = ?timings.ffmpeg_exit_code,
            ffmpeg_stderr_bytes = timings.ffmpeg_stderr_bytes,
            ffmpeg_stderr_truncated = timings.ffmpeg_stderr_truncated,
            ffmpeg_stderr_excerpt = %sanitized_excerpt(&timings.ffmpeg_stderr_tail),
            pcm_stream_elapsed_ms = timings.pcm_stream_elapsed_ms,
            ffmpeg_user_cpu_ms = ?timings.ffmpeg_benchmark.as_ref().and_then(|bench| bench.user_cpu_ms),
            ffmpeg_system_cpu_ms = ?timings.ffmpeg_benchmark.as_ref().and_then(|bench| bench.system_cpu_ms),
            ffmpeg_real_ms = ?timings.ffmpeg_benchmark.as_ref().and_then(|bench| bench.real_ms),
            "【声纹】较慢样本的 FFmpeg 诊断信息"
        );
    }
}
