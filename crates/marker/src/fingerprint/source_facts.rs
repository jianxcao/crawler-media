use std::{path::Path, process::Command, time::Instant};

use super::capture_types::{
    CaptureFailure, CaptureFailureKind, CaptureMetrics, CaptureSourceFacts,
};

/// Parse ffprobe's decimal seconds without floating point or an invented zero origin.
pub fn decimal_seconds_us(value: &str) -> Option<i64> {
    let negative = value.starts_with('-');
    let value = value.strip_prefix('-').unwrap_or(value);
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if fraction.len() > 6 || !fraction.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let micros = whole
        .parse::<i64>()
        .ok()?
        .checked_mul(1_000_000)?
        .checked_add(if fraction.is_empty() {
            0
        } else {
            fraction
                .parse::<i64>()
                .ok()?
                .checked_mul(10i64.pow(6 - fraction.len() as u32))?
        })?;
    if negative {
        micros.checked_neg()
    } else {
        Some(micros)
    }
}

/// Standalone captures resolve once; production passes its existing versioned probe facts.
pub(crate) fn inspect(
    path: &Path,
    selected: Option<u32>,
    deadline_ms: u64,
    control: &crate::WorkControl,
) -> Result<CaptureSourceFacts, CaptureFailure> {
    let started = Instant::now();
    let fail = |message| CaptureFailure {
        kind: CaptureFailureKind::Decode,
        message,
        metrics: CaptureMetrics {
            metadata_elapsed_ms: Some(started.elapsed().as_millis() as u64),
            ..Default::default()
        },
    };
    let _permit = crate::media_read::acquire_media_read(path, control).map_err(fail)?;
    let mut command = Command::new("ffprobe");
    command.args([
        "-v",
        "error",
        "-show_entries",
        "stream=index,codec_type,codec_name,sample_rate,channels,start_pts,time_base:stream_disposition=default:format=start_time",
        "-of",
        "json",
    ]);
    crate::ProbeTarget::from_path(path).apply_input(&mut command);
    let output =
        super::bounded_output::output(&mut command, deadline_ms.max(1), control).map_err(fail)?;
    if !output.status.success() {
        return Err(fail(super::sanitize_stderr(&String::from_utf8_lossy(
            &output.stderr,
        ))));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| fail(e.to_string()))?;
    let streams = json["streams"]
        .as_array()
        .ok_or_else(|| fail("missing audio inventory".into()))?;
    let stream = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .filter(|s| selected.is_none_or(|index| s["index"].as_u64() == Some(index as u64)))
        .min_by_key(|s| {
            (
                s["disposition"]["default"].as_u64() != Some(1),
                s["index"].as_u64(),
            )
        })
        .ok_or_else(|| fail("requested audio stream unavailable".into()))?;
    Ok(CaptureSourceFacts {
        audio_stream_index: stream["index"]
            .as_u64()
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| fail("invalid audio index".into()))?,
        format_start_time_us: json["format"]["start_time"]
            .as_str()
            .and_then(decimal_seconds_us),
        stream_start_pts: stream["start_pts"].as_i64(),
        stream_time_base: stream["time_base"].as_str().map(str::to_owned),
        codec_name: stream["codec_name"].as_str().map(str::to_owned),
        native_sample_rate: stream["sample_rate"]
            .as_str()
            .and_then(|raw| raw.parse().ok()),
        audio_channels: stream["channels"].as_u64().and_then(|n| n.try_into().ok()),
        provenance: "ffprobe_header_v2".into(),
    })
}
