use super::Interval;
use crate::CapturedFingerprint;

/// Proven first-sample bounds in normalized microseconds. Version four never
/// borrows an exact requested start when its source only has millisecond ticks.
pub(super) fn quantized_origin(capture: &CapturedFingerprint) -> Option<(i128, i128)> {
    let mapping = capture.metrics.quantized_mapping.as_ref()?;
    let source = capture.metrics.source_facts.as_ref()?;
    if capture.metrics.time_mapping_version != 4
        || !crate::capture_profile::quantized_aac(source)
        || capture.metrics.audio_stream_index != Some(source.audio_stream_index)
        || capture.metrics.pcm_timeline != crate::PcmTimeline::Contiguous
        || mapping.proof_version != 1
        || Some(mapping.native_sample_rate) != source.native_sample_rate
        || mapping.source_tick_us != 1000
        || mapping.block_anchors < 2
        || mapping.packet_count.checked_mul(1024) != Some(mapping.codec_native_samples)
        || mapping.decoded_native_samples > mapping.codec_native_samples
        || mapping.decoded_native_samples == 0
        || mapping.encoded_payload_sha256.len() != 64
        || mapping.origin_min_us > mapping.origin_max_us
        || mapping.origin_max_us.checked_sub(mapping.origin_min_us)?
            > i64::from(mapping.source_tick_us)
                .checked_mul(2)?
                .checked_add(1)?
        || capture.metrics.sample_count?.checked_mul(2)? != capture.metrics.pcm_bytes
    {
        return None;
    }
    Some((
        i128::from(mapping.origin_min_us),
        i128::from(mapping.origin_max_us),
    ))
}

/// A physical source head has no preceding content to guard. Its first guaranteed
/// integer millisecond still comes from the measured origin, never a tolerance.
pub(super) fn quantized_head_start(capture: &CapturedFingerprint) -> Option<i64> {
    let (min, max) = quantized_origin(capture)?;
    let source = capture.metrics.source_facts.as_ref()?;
    if capture.window.start_ms != 0
        || source.format_start_time_us != Some(0)
        || source.stream_start_pts != Some(0)
        || min > 0
        || max < 0
    {
        return None;
    }
    i64::try_from((max + 999).div_euclid(1000)).ok()
}

pub(super) fn quantized_coverage(capture: &CapturedFingerprint) -> Option<Interval> {
    let (min, max) = quantized_origin(capture)?;
    let count = i128::from(capture.metrics.sample_count?);
    Some(Interval {
        start_ms: i64::try_from((max + 999).div_euclid(1000))
            .ok()?
            .max(capture.window.start_ms),
        end_ms: i64::try_from((min * 16 + count * 1000).div_euclid(16_000))
            .ok()?
            .min(capture.window.end_ms),
    })
}

/// Conservative intersection of every coordinate admitted by the measured
/// origin interval. Fine-clock producers retain their existing exact mapping.
pub fn normalize_capture_interval(
    capture: &CapturedFingerprint,
    relative: Interval,
) -> Result<Interval, String> {
    let exact = super::normalize_interval(&capture.window, relative)?;
    if capture.metrics.time_mapping_version != 4 {
        return Ok(exact);
    }
    let (min, max) = quantized_origin(capture).ok_or("missing quantized origin proof")?;
    let start = (max + i128::from(relative.start_ms) * 1000 + 999).div_euclid(1000);
    let end = (min + i128::from(relative.end_ms) * 1000).div_euclid(1000);
    let interval = Interval {
        start_ms: i64::try_from(start).map_err(|_| "mapped start overflow")?,
        end_ms: i64::try_from(end).map_err(|_| "mapped end overflow")?,
    };
    if interval.start_ms < 0
        || interval.end_ms <= interval.start_ms
        || interval.start_ms < capture.window.start_ms
        || interval.end_ms > capture.window.end_ms
    {
        return Err("quantized candidate outside actual window".into());
    }
    Ok(interval)
}
