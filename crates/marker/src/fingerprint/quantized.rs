//! Narrow AAC-LC/Matroska path: one source demux owns both the proof and bytes.
mod ledger;
mod pcm;
use super::{
    CaptureFailure, CaptureFailureKind, CaptureMetrics, CaptureRequest, CaptureSourceFacts,
    CapturedFingerprint,
};
use std::{process::Command, time::Instant};

const MAX_PROOF_BYTES: usize = 32 * 1024 * 1024;
fn preroll_ms(rate: u32) -> i64 {
    (3 * 1024 * 1000i64 + i64::from(rate) - 1) / i64::from(rate)
}

pub(super) fn capture(
    request: &CaptureRequest,
    source: &CaptureSourceFacts,
    control: &crate::WorkControl,
    started: Instant,
) -> Result<CapturedFingerprint, CaptureFailure> {
    let gate_ms = started.elapsed().as_millis() as u64;
    let process_started = Instant::now();
    let fail = |message: String| CaptureFailure {
        kind: if control.is_cancelled() {
            CaptureFailureKind::Cancelled
        } else if message.contains("deadline exceeded") {
            CaptureFailureKind::Timeout
        } else {
            CaptureFailureKind::Decode
        },
        message: format!("unsupported_aac_packet_timing: {message}"),
        metrics: CaptureMetrics {
            source_facts: Some(source.clone()),
            time_mapping_version: 4,
            remote_gate_wait_ms: Some(gate_ms),
            capture_process_ms: Some(process_started.elapsed().as_millis() as u64),
            ..Default::default()
        },
    };
    let rate = source
        .native_sample_rate
        .filter(|rate| matches!(rate, 44_100 | 48_000))
        .ok_or_else(|| fail("unsupported native sample rate".into()))?;
    let preroll_ms = preroll_ms(rate);
    let origin = source
        .format_start_time_us
        .ok_or_else(|| fail("missing format origin".into()))?;
    let deadline = request.process_deadline_ms;
    if deadline == 0 {
        return Err(fail("finite capture deadline required".into()));
    }
    // Three AAC-LC frames admit one measured frame of overlap state plus a full
    // frame before the selected start. The ledger verifies the actual seek.
    let start_us = origin
        .checked_add(
            request
                .window
                .start_ms
                .saturating_sub(preroll_ms)
                .max(0)
                .checked_mul(1000)
                .ok_or_else(|| fail("seek overflow".into()))?,
        )
        .ok_or_else(|| fail("seek overflow".into()))?;
    let end_us = origin
        .checked_add(
            request
                .window
                .end_ms
                .checked_mul(1000)
                .ok_or_else(|| fail("end overflow".into()))?,
        )
        .ok_or_else(|| fail("end overflow".into()))?;
    let interval = if request.window.start_ms <= preroll_ms {
        format!("%{:.6}", end_us as f64 / 1e6)
    } else {
        format!("{:.6}%{:.6}", start_us as f64 / 1e6, end_us as f64 / 1e6)
    };
    let mut command = Command::new("ffprobe");
    command.args(["-v", "error", "-threads", "1", "-select_streams", &source.audio_stream_index.to_string(),
        "-read_intervals", &interval,
        "-show_packets", "-show_frames", "-show_streams", "-show_format", "-show_data",
        "-show_entries", "packet=stream_index,pts,pos,size,data:packet_side_data=side_data_type,skip_samples,discard_padding:frame=stream_index,pts,pkt_pos,pkt_size,nb_samples:stream=index,codec_name,profile,sample_rate,channels,time_base,extradata,initial_padding:format=format_name,start_time", "-of", "json"]);
    crate::ProbeTarget::from_path(&request.path).apply_input(&mut command);
    let output = super::bounded_output::output_with_input(
        &mut command,
        deadline,
        control,
        None,
        MAX_PROOF_BYTES,
    )
    .map_err(fail)?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(fail("source demux/decode failed".into()));
    }
    let proof = ledger::prepare(&output.stdout, source).map_err(fail)?;
    let remaining = deadline
        .checked_sub(process_started.elapsed().as_millis() as u64)
        .filter(|remaining| *remaining > 0)
        .ok_or_else(|| fail("capture deadline exceeded".into()))?;
    let mut captured = pcm::decode(request, source, proof, remaining, control).map_err(fail)?;
    captured.metrics.remote_gate_wait_ms = Some(gate_ms);
    captured.metrics.capture_process_ms = Some(process_started.elapsed().as_millis() as u64);
    captured.metrics.elapsed_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        audio_stream_index = source.audio_stream_index,
        start_ms = request.window.start_ms,
        end_ms = request.window.end_ms,
        elapsed_ms = captured.metrics.elapsed_ms,
        "【声纹】同次包证据与本地 AAC 解码完成"
    );
    Ok(captured)
}
