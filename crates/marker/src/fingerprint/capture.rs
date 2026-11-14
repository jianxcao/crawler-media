use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rusty_chromaprint::Fingerprinter;
use sha2::Digest;

use super::capture_types::{
    CaptureFailure, CaptureFailureKind, CaptureMetrics, CaptureRequest, CapturedFingerprint,
};
use super::chromaprint::{
    CapturedStderr, ExtractionTimings, ffmpeg_exit_error, ffmpeg_log_level, log_extraction_timings,
    start_fingerprinter,
};
use crate::target::ProbeTarget;

pub fn capture_window_chromaprint(
    request: &CaptureRequest,
) -> Result<CapturedFingerprint, CaptureFailure> {
    capture_window_controlled(request, &crate::WorkControl::default())
}

pub fn capture_window_controlled(
    request: &CaptureRequest,
    control: &crate::WorkControl,
) -> Result<CapturedFingerprint, CaptureFailure> {
    request.window.validate()?;
    let metadata_started = Instant::now();
    let source = super::source_facts::inspect(
        &request.path,
        request.audio_stream_index,
        if request.process_deadline_ms == 0 {
            120_000
        } else {
            request.process_deadline_ms
        },
        control,
    )?;
    let metadata_elapsed = metadata_started.elapsed().as_millis() as u64;
    let result = capture_from_source(request, &source, control, std::ffi::OsStr::new("ffmpeg"));
    result
        .map(|mut capture| {
            capture.metrics.metadata_elapsed_ms = Some(metadata_elapsed);
            capture
        })
        .map_err(|mut error| {
            error.metrics.metadata_elapsed_ms = Some(metadata_elapsed);
            error
        })
}

/// The production metadata stage supplies these same-source facts; no metadata IO here.
pub fn capture_from_source(
    request: &CaptureRequest,
    source: &super::CaptureSourceFacts,
    control: &crate::WorkControl,
    program: &std::ffi::OsStr,
) -> Result<CapturedFingerprint, CaptureFailure> {
    let started = Instant::now();
    if control.is_cancelled() {
        return Err(CaptureFailure {
            kind: CaptureFailureKind::Cancelled,
            message: "Library work cancelled".into(),
            metrics: CaptureMetrics::default(),
        });
    }
    request.window.validate()?;
    if request
        .audio_stream_index
        .is_some_and(|index| index != source.audio_stream_index)
    {
        return Err(CaptureFailure {
            kind: CaptureFailureKind::Decode,
            message: "capture source audio selection mismatch".into(),
            metrics: CaptureMetrics::default(),
        });
    }
    let _permit =
        crate::media_read::acquire_media_read(&request.path, control).map_err(|message| {
            CaptureFailure {
                kind: CaptureFailureKind::Cancelled,
                message,
                metrics: CaptureMetrics::default(),
            }
        })?;

    if crate::capture_profile::quantized_aac(source) {
        return super::quantized::capture(request, source, control, started);
    }

    let mut timings = ExtractionTimings {
        ffmpeg_log_level: ffmpeg_log_level().to_string(),
        remote_gate_wait_ms: started.elapsed().as_millis() as u64,
        source_facts: Some(source.clone()),
        ..Default::default()
    };

    let mut fingerprinter = match start_fingerprinter() {
        Ok(fp) => fp,
        Err(err) => {
            return Err(CaptureFailure {
                kind: CaptureFailureKind::Decode,
                message: err,
                metrics: build_metrics(&timings, started, None),
            });
        }
    };

    let setup_started = Instant::now();
    let command = build_capture_ffmpeg_command(request, source, program, &timings.ffmpeg_log_level);
    timings.command_setup_ms = setup_started.elapsed().as_millis();

    let mut child = match spawn_ffmpeg(command, &mut timings) {
        Ok(c) => c,
        Err(err) => {
            return Err(CaptureFailure {
                kind: CaptureFailureKind::Io,
                message: err,
                metrics: build_metrics(&timings, started, None),
            });
        }
    };

    let run_res = run_ffmpeg_with_deadline(
        &mut child,
        &mut fingerprinter,
        &mut timings,
        request.process_deadline_ms,
        control,
    );

    let finish_started = Instant::now();
    fingerprinter.finish();
    timings.chromaprint_finish_ms = finish_started.elapsed().as_millis();

    finish_captured_fingerprint(request, run_res, &mut fingerprinter, timings, started)
}

fn finish_captured_fingerprint(
    request: &CaptureRequest,
    run_res: Result<(), CaptureFailure>,
    fingerprinter: &mut Fingerprinter,
    timings: ExtractionTimings,
    started: Instant,
) -> Result<CapturedFingerprint, CaptureFailure> {
    let source = match ProbeTarget::from_path(&request.path) {
        ProbeTarget::Local(_) => "local",
        ProbeTarget::Remote(_) => "remote",
    };

    let duration_secs = (request.window.duration_ms() as f64 / 1000.0).ceil() as u32;
    let start_secs = (request.window.start_ms as f64 / 1000.0).floor() as u32;

    match run_res {
        Ok(_) => {
            let words = fingerprinter.fingerprint().to_vec();
            if words.is_empty() {
                let metrics = build_metrics(&timings, started, None);
                let err_res =
                    Err("Empty fingerprint extracted (ffmpeg produced no audio samples)".into());
                log_extraction_timings(
                    &request.path,
                    source,
                    start_secs,
                    duration_secs,
                    started,
                    &timings,
                    &err_res,
                );
                return Err(CaptureFailure {
                    kind: CaptureFailureKind::EmptyAudio,
                    message: "Empty fingerprint extracted (ffmpeg produced no audio samples)"
                        .into(),
                    metrics,
                });
            }

            let ok_res = Ok(words.clone());
            log_extraction_timings(
                &request.path,
                source,
                start_secs,
                duration_secs,
                started,
                &timings,
                &ok_res,
            );

            let mut metrics = build_metrics(&timings, started, None);
            metrics.audio_stream_index = Some(source_index(&timings));
            Ok(CapturedFingerprint {
                window: request.window.clone(),
                words,
                pcm_duration_ms: Some((timings.sample_count / 16) as i64),
                metrics,
            })
        }
        Err(failure) => {
            let err_res = Err(failure.message.clone());
            log_extraction_timings(
                &request.path,
                source,
                start_secs,
                duration_secs,
                started,
                &timings,
                &err_res,
            );
            Err(failure)
        }
    }
}

fn source_index(timings: &ExtractionTimings) -> u32 {
    timings
        .source_facts
        .as_ref()
        .map(|s| s.audio_stream_index)
        .unwrap_or(u32::MAX)
}

fn build_capture_ffmpeg_command(
    request: &CaptureRequest,
    source: &super::CaptureSourceFacts,
    program: &std::ffi::OsStr,
    log_level: &str,
) -> Command {
    let target = ProbeTarget::from_path(&request.path);
    let mut command = Command::new(program);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-nostats",
        "-benchmark",
        "-v",
        log_level,
        "-filter_threads",
        "1",
        "-threads",
        "1",
        "-copyts",
        "-accurate_seek",
    ]);

    target.apply_ffmpeg_input_with_seek_ms(&mut command, request.window.start_ms);

    command.args(["-map", &format!("0:{}", source.audio_stream_index)]);
    let trim = match source.format_start_time_us {
        Some(origin) => format!(
            "start={:.6}:end={:.6}",
            origin as f64 / 1_000_000.0 + request.window.start_ms as f64 / 1000.0,
            origin as f64 / 1_000_000.0 + request.window.end_ms as f64 / 1000.0
        ),
        // Unknown format origin cannot authorize boundary evidence. Still bound this decode.
        None => format!(
            "duration={:.3}",
            request.window.duration_ms() as f64 / 1000.0
        ),
    };
    let filter = format!(
        "aresample=16000:async=0,aformat=sample_fmts=s16:channel_layouts=mono,atrim={trim},ashowinfo@cm_timeline"
    );
    command.args([
        "-vn",
        "-sn",
        "-dn",
        "-af",
        &filter,
        "-c:a",
        "pcm_s16le",
        "-threads",
        "1",
        "-f",
        "s16le",
        "-",
    ]);

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command
}

fn spawn_ffmpeg(mut command: Command, timings: &mut ExtractionTimings) -> Result<Child, String> {
    let spawn_started = Instant::now();
    let spawn_result = command.spawn();
    timings.ffmpeg_spawn_ms = spawn_started.elapsed().as_millis();
    spawn_result.map_err(|error| error.to_string())
}

fn spawn_stdout_reader(
    stdout: std::process::ChildStdout,
) -> (
    mpsc::Receiver<Vec<u8>>,
    std::thread::JoinHandle<Result<(), String>>,
) {
    let (tx, rx) = mpsc::sync_channel(4);
    let handle = std::thread::spawn(move || {
        use std::io::Read;
        let mut reader = stdout;
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(chunk[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Vec::new());
                    return Err(e.to_string());
                }
            }
        }
        Ok(())
    });
    (rx, handle)
}

fn finalize_ffmpeg_exit(
    child: &mut Child,
    stderr_reader: std::thread::JoinHandle<Result<CapturedStderr, String>>,
    timings: &mut ExtractionTimings,
    pending_low_byte: Option<u8>,
    process_started: Instant,
) -> Result<(), CaptureFailure> {
    let wait_started = Instant::now();
    let status_res = child.wait();
    timings.ffmpeg_wait_ms = wait_started.elapsed().as_millis();

    let stderr = collect_stderr_to_timings(stderr_reader, timings)?;

    let status = match status_res {
        Ok(s) => s,
        Err(e) => {
            return Err(CaptureFailure {
                kind: CaptureFailureKind::Io,
                message: format!("Failed waiting for ffmpeg: {e}"),
                metrics: build_metrics(timings, process_started, None),
            });
        }
    };

    timings.ffmpeg_exit_code = status.code();

    if pending_low_byte.is_some() {
        return Err(CaptureFailure {
            kind: CaptureFailureKind::Decode,
            message: "ffmpeg produced an incomplete 16-bit PCM sample".into(),
            metrics: build_metrics(timings, process_started, None),
        });
    }

    if !status.success() {
        return Err(CaptureFailure {
            kind: CaptureFailureKind::Decode,
            message: ffmpeg_exit_error(status, &stderr.text),
            metrics: build_metrics(timings, process_started, None),
        });
    }

    Ok(())
}

fn run_ffmpeg_with_deadline(
    child: &mut Child,
    fingerprinter: &mut Fingerprinter,
    timings: &mut ExtractionTimings,
    deadline_ms: u64,
    control: &crate::WorkControl,
) -> Result<(), CaptureFailure> {
    let process_started = Instant::now();
    let stdout = child.stdout.take().ok_or_else(|| CaptureFailure {
        kind: CaptureFailureKind::Io,
        message: "Failed to capture ffmpeg stdout".into(),
        metrics: build_metrics(timings, process_started, None),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| CaptureFailure {
        kind: CaptureFailureKind::Io,
        message: "Failed to capture ffmpeg stderr".into(),
        metrics: build_metrics(timings, process_started, None),
    })?;

    let stderr_reader =
        std::thread::spawn(move || super::chromaprint::diagnostics::read_stderr_tail(stderr));

    let (rx, stdout_join) = spawn_stdout_reader(stdout);

    let deadline = Duration::from_millis(deadline_ms);
    let mut pending_low_byte = None;
    let mut samples = Vec::new();
    let mut timed_out = false;

    loop {
        if control.is_cancelled() {
            break;
        }
        let elapsed = process_started.elapsed();
        if deadline_ms > 0 && elapsed >= deadline {
            timed_out = true;
            break;
        }

        let timeout_remaining = if deadline_ms > 0 {
            deadline.saturating_sub(elapsed)
        } else {
            Duration::from_secs(3600)
        };

        let wait_start = Instant::now();
        match rx.recv_timeout(timeout_remaining.min(Duration::from_millis(100))) {
            Ok(bytes) => {
                timings.pcm_read_wait += wait_start.elapsed();
                if bytes.is_empty() {
                    break;
                }
                timings
                    .time_to_first_pcm_ms
                    .get_or_insert_with(|| process_started.elapsed().as_millis());
                timings.pcm_bytes += bytes.len() as u64;
                timings.pcm_sha256.update(&bytes);

                decode_pcm_chunk_ref(&bytes, &mut pending_low_byte, &mut samples);
                let consume_started = Instant::now();
                fingerprinter.consume(&samples);
                timings.pcm_rms.consume(&samples);
                timings.chromaprint_consume += consume_started.elapsed();
                timings.sample_count += samples.len() as u64;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if deadline_ms > 0 && process_started.elapsed() >= deadline {
                    timed_out = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
        }
    }

    timings.pcm_stream_elapsed_ms = process_started.elapsed().as_millis();
    timings.process_elapsed_ms = Some(process_started.elapsed().as_millis() as u64);

    if timed_out || control.is_cancelled() {
        let cancelled = control.is_cancelled();
        let _ = child.kill();
        let _ = child.wait();
        drop(rx);
        let _ = stdout_join.join();
        let _ = collect_stderr_to_timings(stderr_reader, timings);
        return Err(CaptureFailure {
            kind: if cancelled {
                CaptureFailureKind::Cancelled
            } else {
                CaptureFailureKind::Timeout
            },
            message: if cancelled {
                "Library work cancelled".into()
            } else {
                format!("FFmpeg process exceeded deadline of {deadline_ms}ms")
            },
            metrics: build_metrics(timings, process_started, None),
        });
    }

    let _ = stdout_join.join();

    finalize_ffmpeg_exit(
        child,
        stderr_reader,
        timings,
        pending_low_byte,
        process_started,
    )
}

fn decode_pcm_chunk_ref(bytes: &[u8], pending_low_byte: &mut Option<u8>, samples: &mut Vec<i16>) {
    samples.clear();
    let mut start = 0;
    if let Some(low_byte) = pending_low_byte.take() {
        samples.push(i16::from_le_bytes([low_byte, bytes[0]]));
        start = 1;
    }
    let aligned_length = (bytes.len() - start) & !1;
    for sample in bytes[start..start + aligned_length].as_chunks::<2>().0 {
        samples.push(i16::from_le_bytes([sample[0], sample[1]]));
    }
    if start + aligned_length < bytes.len() {
        *pending_low_byte = Some(bytes[bytes.len() - 1]);
    }
}

fn collect_stderr_to_timings(
    reader: std::thread::JoinHandle<Result<CapturedStderr, String>>,
    timings: &mut ExtractionTimings,
) -> Result<CapturedStderr, CaptureFailure> {
    let started = Instant::now();
    let stderr = reader
        .join()
        .map_err(|_| "ffmpeg stderr reader panicked".to_string())
        .and_then(|res| res)
        .map_err(|err| CaptureFailure {
            kind: CaptureFailureKind::Io,
            message: err,
            metrics: build_metrics(timings, started, None),
        })?;

    timings.stderr_collect_ms = started.elapsed().as_millis();
    timings.ffmpeg_stderr_bytes = stderr.total_bytes;
    timings.ffmpeg_stderr_truncated = stderr.truncated;
    timings.ffmpeg_benchmark = stderr.benchmark.clone();
    timings.timeline = stderr.timeline.clone();
    timings.ffmpeg_stderr_tail = stderr.text.clone();
    Ok(stderr)
}

fn build_metrics(
    timings: &ExtractionTimings,
    started: Instant,
    input_bytes: Option<u64>,
) -> CaptureMetrics {
    CaptureMetrics {
        quantized_mapping: None,
        captured_job_id: None,
        pcm_frame_ms: 20,
        pcm_rms_dbfs_milli: timings.pcm_rms.frames.clone(),
        audio_stream_index: timings.source_facts.as_ref().map(|s| s.audio_stream_index),
        source_start_pts_ms: None,
        contiguous_pcm_start_ms: normalized_origin(timings),
        pcm_timeline: timings
            .timeline
            .state(timings.sample_count, timings.ffmpeg_exit_code == Some(0)),
        first_decoded_source_pts: timings.timeline.first_pts,
        decoded_time_base: timings.timeline.first_pts.map(|_| "1/16000".into()),
        source_facts: timings.source_facts.clone(),
        remote_gate_wait_ms: Some(timings.remote_gate_wait_ms),
        capture_process_ms: timings.process_elapsed_ms,
        metadata_elapsed_ms: Some(0),
        time_mapping_version: 3,
        elapsed_ms: started.elapsed().as_millis() as u64,
        time_to_first_pcm_ms: timings.time_to_first_pcm_ms.map(|t| t as u64),
        pcm_read_wait_us: timings.pcm_read_wait.as_micros() as u64,
        chromaprint_consume_us: timings.chromaprint_consume.as_micros() as u64,
        pcm_bytes: timings.pcm_bytes,
        pcm_sha256: Some(format!("{:x}", timings.pcm_sha256.clone().finalize())),
        input_bytes,
        input_bytes_source: None,
        measurement_complete: false,
        ffmpeg_exit_code: timings.ffmpeg_exit_code,

        command_setup_ms: Some(timings.command_setup_ms as u64),
        ffmpeg_spawn_ms: Some(timings.ffmpeg_spawn_ms as u64),
        pcm_stream_elapsed_ms: Some(timings.pcm_stream_elapsed_ms as u64),
        chromaprint_finish_ms: Some(timings.chromaprint_finish_ms as u64),
        ffmpeg_wait_ms: Some(timings.ffmpeg_wait_ms as u64),
        stderr_collect_ms: Some(timings.stderr_collect_ms as u64),
        sample_count: Some(timings.sample_count),
        ffmpeg_user_cpu_ms: timings
            .ffmpeg_benchmark
            .as_ref()
            .and_then(|b| b.user_cpu_ms)
            .map(|v| v as u64),
        ffmpeg_system_cpu_ms: timings
            .ffmpeg_benchmark
            .as_ref()
            .and_then(|b| b.system_cpu_ms)
            .map(|v| v as u64),
        ffmpeg_real_ms: timings
            .ffmpeg_benchmark
            .as_ref()
            .and_then(|b| b.real_ms)
            .map(|v| v as u64),
        ffmpeg_maxrss_kb: timings.ffmpeg_benchmark.as_ref().and_then(|b| b.max_rss_kb),
        ffmpeg_stderr_bytes: Some(timings.ffmpeg_stderr_bytes),
        ffmpeg_stderr_tail: Some(super::chromaprint::diagnostics::sanitized_excerpt(
            &timings.ffmpeg_stderr_tail,
        )),
    }
}

fn normalized_origin(timings: &ExtractionTimings) -> Option<i64> {
    if timings
        .timeline
        .state(timings.sample_count, timings.ffmpeg_exit_code == Some(0))
        != super::PcmTimeline::Contiguous
    {
        return None;
    }
    let origin_us = timings.source_facts.as_ref()?.format_start_time_us?;
    // Exact sample clock and exact microsecond header clock: do not round sub-ms alignment.
    let normalized = i128::from(timings.timeline.first_pts?)
        .checked_mul(1_000_000)?
        .checked_sub(i128::from(origin_us).checked_mul(16_000)?)?;
    (normalized % 16_000_000 == 0)
        .then(|| i64::try_from(normalized / 16_000_000).ok())
        .flatten()
}

/// Streaming 20ms mono RMS frames; incomplete trailing frames are not evidence.
#[derive(Clone, Debug, Default)]
pub(crate) struct PcmRms {
    frames: Vec<i32>,
    sum_squares: f64,
    samples: usize,
}
impl PcmRms {
    pub(super) fn consume(&mut self, pcm: &[i16]) {
        for sample in pcm {
            self.sum_squares += f64::from(*sample).powi(2);
            self.samples += 1;
            if self.samples == 320 {
                let power = self.sum_squares / 320.0 / 32768f64.powi(2);
                self.frames
                    .push((10_000.0 * power.max(1e-12).log10()).round() as i32);
                self.sum_squares = 0.0;
                self.samples = 0;
            }
        }
    }

    pub(super) fn frames(self) -> Vec<i32> {
        self.frames
    }
}
