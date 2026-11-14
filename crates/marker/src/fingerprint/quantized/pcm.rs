use super::super::{
    CaptureMetrics, CaptureRequest, CaptureSourceFacts, CapturedFingerprint, PcmTimeline,
    QuantizedPcmMapping,
};
use super::ledger::Proof;
use sha2::{Digest, Sha256};
use std::process::Command;

pub(super) fn decode(
    request: &CaptureRequest,
    source: &CaptureSourceFacts,
    proof: Proof,
    deadline: u64,
    control: &crate::WorkControl,
) -> Result<CapturedFingerprint, String> {
    let rate = i128::from(proof.rate);
    let expected_native = proof
        .packets
        .checked_mul(1024)
        .ok_or("native count overflow")?;
    let (phase_min, phase_max) = proof.origin_bounds_us();
    let raw_end = expected_native
        .checked_sub(proof.discard)
        .ok_or("discard exceeds native PCM")?;
    let format_us = i128::from(source.format_start_time_us.ok_or("missing format origin")?);
    let first_min = phase_min + i128::from(proof.skip) * 1_000_000;
    let first_max = phase_max + i128::from(proof.skip) * 1_000_000;
    let midpoint = (first_min + first_max) / 2;
    let wanted = (format_us + i128::from(request.window.start_ms) * 1000) * rate;
    // A head request retains the first measured, padding-trimmed sample. Its
    // origin remains the measured interval, including negative/leading offsets.
    let output_start = if request.window.start_ms == 0 {
        0
    } else {
        ((wanted - midpoint) * 16_000)
            .div_euclid(rate * 1_000_000)
            .max(0)
    };
    // AAC-LC synthesis uses the preceding 1024-sample overlap. An interior
    // capture must measure two whole owned access units before its first sample.
    if request.window.start_ms > super::preroll_ms(proof.rate)
        && output_start * rate < 2048 * 16_000
    {
        return Err("insufficient measured AAC overlap preroll".into());
    }
    let requested_samples = i128::from(request.window.duration_ms()) * 16;
    let available = (i128::from(raw_end - proof.skip) * 16_000).div_euclid(rate);
    let desired_end = (output_start + requested_samples).min(available);
    if desired_end <= output_start {
        return Err("no PCM within selected window".into());
    }
    // Observe native AAC frames before trimming, in the same local decode that
    // resamples the owned bytes. Keeping fltp avoids an intermediate remix path.
    let filter = format!(
        "ashowinfo@cm_native,atrim=start_sample={}:end_sample={raw_end},aresample=16000:async=0,aformat=sample_fmts=s16:channel_layouts=mono,atrim=start_sample={output_start}:end_sample={desired_end}",
        proof.skip
    );
    let mut resampler = Command::new("ffmpeg");
    resampler.args([
        "-v",
        "info",
        "-nostdin",
        "-filter_threads",
        "1",
        "-threads",
        "1",
        "-f",
        "aac",
        "-i",
        "pipe:0",
        "-af",
        &filter,
        "-c:a",
        "pcm_s16le",
        "-threads",
        "1",
        "-f",
        "s16le",
        "pipe:1",
    ]);
    let output = super::super::bounded_output::output_with_limits(
        &mut resampler,
        deadline,
        control,
        Some(proof.adts),
        16 * 1024 * 1024,
        8 * 1024 * 1024,
    )?;
    if !output.status.success() || output.stdout.len() % 2 != 0 {
        return Err("owned PCM resampling failed".into());
    }
    let required_native =
        u64::try_from(i128::from(proof.skip) + (desired_end * rate + 15_999) / 16_000)
            .map_err(|_| "native count overflow")?;
    let observed_native =
        verify_native(&output.stderr, expected_native, required_native, proof.rate)?;
    let count = output.stdout.len() as u64 / 2;
    if count as i128 != desired_end - output_start {
        return Err("resampled sample count mismatch".into());
    }
    // Bounds are outward-rounded, never converted into a fabricated exact origin.
    let shift = output_start * rate * 1_000_000 - format_us * rate * 16_000;
    let denominator = rate * 16_000;
    let origin_min = (first_min * 16_000 + shift).div_euclid(denominator);
    let origin_max = (first_max * 16_000 + shift + denominator - 1).div_euclid(denominator);
    let mapping = QuantizedPcmMapping {
        origin_min_us: origin_min.try_into().map_err(|_| "origin overflow")?,
        origin_max_us: origin_max.try_into().map_err(|_| "origin overflow")?,
        native_sample_rate: proof.rate,
        source_tick_us: 1000,
        packet_count: proof.packets,
        block_anchors: proof.anchors,
        decoded_native_samples: observed_native,
        codec_native_samples: expected_native,
        encoded_payload_sha256: proof.payload_sha256,
        proof_version: 1,
    };
    finish(request, source, output.stdout, mapping)
}

fn verify_native(stderr: &[u8], expected: u64, required: u64, rate: u32) -> Result<u64, String> {
    let rate_field = format!("rate:{rate}");
    let stderr = std::str::from_utf8(stderr).map_err(|_| "malformed native observation")?;
    let mut frames = 0;
    for line in stderr
        .lines()
        .filter(|line| line.starts_with("[ashowinfo@cm_native @ "))
    {
        let (_, fields) = line.split_once("] ").ok_or("malformed native frame")?;
        let fields = fields.split_whitespace().collect::<Vec<_>>();
        if fields
            .first()
            .and_then(|raw| raw.strip_prefix("n:"))
            .and_then(|n| n.parse::<u64>().ok())
            != Some(frames)
            || !fields.contains(&"fmt:fltp")
            || !fields.contains(&"channels:2")
            || !fields.contains(&rate_field.as_str())
            || !fields.contains(&"nb_samples:1024")
        {
            return Err("owned native AAC sample observations changed".into());
        }
        frames += 1;
    }
    // atrim stops reading once its selected output is complete. The actual
    // observed prefix must cover every sample consumed, not an invented EOF.
    if frames * 1024 < required || frames * 1024 > expected {
        return Err("missing native sample observations".into());
    }
    Ok(frames * 1024)
}

fn finish(
    request: &CaptureRequest,
    source: &CaptureSourceFacts,
    bytes: Vec<u8>,
    mapping: QuantizedPcmMapping,
) -> Result<CapturedFingerprint, String> {
    let pcm = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    let mut printer = super::super::chromaprint::start_fingerprinter()?;
    printer.consume(&pcm);
    printer.finish();
    let words = printer.fingerprint().to_vec();
    if words.is_empty() {
        return Err("empty measured fingerprint".into());
    }
    let mut rms = super::super::capture::PcmRms::default();
    rms.consume(&pcm);
    Ok(CapturedFingerprint {
        window: request.window.clone(),
        words,
        pcm_duration_ms: Some(pcm.len() as i64 / 16),
        metrics: CaptureMetrics {
            quantized_mapping: Some(mapping),
            pcm_timeline: PcmTimeline::Contiguous,
            contiguous_pcm_start_ms: None,
            source_facts: Some(source.clone()),
            audio_stream_index: Some(source.audio_stream_index),
            time_mapping_version: 4,
            pcm_frame_ms: 20,
            pcm_rms_dbfs_milli: rms.frames(),
            sample_count: Some(pcm.len() as u64),
            pcm_bytes: bytes.len() as u64,
            pcm_sha256: Some(format!("{:x}", Sha256::digest(&bytes))),
            ffmpeg_exit_code: Some(0),
            ..Default::default()
        },
    })
}
