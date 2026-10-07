use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use rusty_chromaprint::{Configuration, Fingerprinter};

use crate::target::ProbeTarget;

mod diagnostics;
mod pcm;
mod timing;

use diagnostics::{CapturedStderr, parse_benchmark, read_stderr_tail, sanitized_excerpt};
use pcm::consume_pcm;
use timing::{ExtractionTimings, log_extraction_timings};

pub(super) fn extract(
    path: &Path,
    start_secs: u32,
    duration_secs: u32,
) -> Result<Vec<u32>, String> {
    let started = Instant::now();
    let source = match ProbeTarget::from_path(path) {
        ProbeTarget::Local(_) => "local",
        ProbeTarget::Remote(_) => "remote",
    };
    let mut timings = ExtractionTimings::default();
    let result = extract_fingerprint(path, start_secs, duration_secs, &mut timings);
    log_extraction_timings(
        path,
        source,
        start_secs,
        duration_secs,
        started,
        &timings,
        &result,
    );
    result
}

fn extract_fingerprint(
    path: &Path,
    start_secs: u32,
    duration_secs: u32,
    timings: &mut ExtractionTimings,
) -> Result<Vec<u32>, String> {
    let setup_started = Instant::now();
    let mut fingerprinter = start_fingerprinter()?;
    let log_level = ffmpeg_log_level();
    timings.ffmpeg_log_level = log_level.to_string();
    let command = build_ffmpeg_command(path, start_secs, duration_secs, log_level);
    timings.command_setup_ms = setup_started.elapsed().as_millis();

    run_ffmpeg(command, &mut fingerprinter, timings)?;

    let finish_started = Instant::now();
    fingerprinter.finish();
    timings.chromaprint_finish_ms = finish_started.elapsed().as_millis();
    let fingerprint = fingerprinter.fingerprint().to_vec();
    if fingerprint.is_empty() {
        return Err("Empty fingerprint extracted (ffmpeg produced no audio samples)".into());
    }
    Ok(fingerprint)
}

fn start_fingerprinter() -> Result<Fingerprinter, String> {
    let config = Configuration::preset_test2();
    let mut fingerprinter = Fingerprinter::new(&config);
    fingerprinter
        .start(16000, 1)
        .map_err(|error| format!("fingerprinter start failed: {error:?}"))?;
    Ok(fingerprinter)
}

fn build_ffmpeg_command(
    path: &Path,
    start_secs: u32,
    duration_secs: u32,
    log_level: &str,
) -> Command {
    let target = ProbeTarget::from_path(path);
    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-nostats", "-benchmark", "-v", log_level]);
    target.apply_ffmpeg_input_with_seek(&mut command, start_secs);
    command.args([
        "-t",
        &duration_secs.to_string(),
        "-vn",
        "-ac",
        "1",
        "-ar",
        "16000",
        "-f",
        "s16le",
        "-",
    ]);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command
}

fn run_ffmpeg(
    mut command: Command,
    fingerprinter: &mut Fingerprinter,
    timings: &mut ExtractionTimings,
) -> Result<(), String> {
    let spawn_started = Instant::now();
    let spawn_result = command.spawn();
    timings.ffmpeg_spawn_ms = spawn_started.elapsed().as_millis();
    let mut child = spawn_result.map_err(|error| error.to_string())?;
    let process_started = Instant::now();
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Failed to capture ffmpeg stdout".into());
    };
    let Some(stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Failed to capture ffmpeg stderr".into());
    };
    let stderr_reader = std::thread::spawn(move || read_stderr_tail(stderr));

    let read_result = consume_pcm(stdout, fingerprinter, process_started, timings);
    if read_result.is_err() {
        let _ = child.kill();
    }
    let status = wait_for_ffmpeg(&mut child, timings)?;
    timings.ffmpeg_exit_code = status.code();
    let stderr = collect_stderr(stderr_reader, timings)?;
    read_result?;
    if !status.success() {
        return Err(ffmpeg_exit_error(status, &stderr.text));
    }
    Ok(())
}

fn wait_for_ffmpeg(
    child: &mut Child,
    timings: &mut ExtractionTimings,
) -> Result<std::process::ExitStatus, String> {
    let started = Instant::now();
    let status = child.wait().map_err(|error| error.to_string());
    timings.ffmpeg_wait_ms = started.elapsed().as_millis();
    status
}

fn collect_stderr(
    reader: std::thread::JoinHandle<Result<CapturedStderr, String>>,
    timings: &mut ExtractionTimings,
) -> Result<CapturedStderr, String> {
    let started = Instant::now();
    let stderr = reader
        .join()
        .map_err(|_| "ffmpeg stderr reader panicked".to_string())??;
    timings.stderr_collect_ms = started.elapsed().as_millis();
    timings.ffmpeg_stderr_bytes = stderr.total_bytes;
    timings.ffmpeg_stderr_truncated = stderr.truncated;
    timings.ffmpeg_benchmark = parse_benchmark(&stderr.text);
    timings.ffmpeg_stderr_tail = stderr.text.clone();
    Ok(stderr)
}

fn ffmpeg_exit_error(status: std::process::ExitStatus, stderr: &str) -> String {
    let detail = sanitized_excerpt(stderr);
    if detail.is_empty() {
        format!("ffmpeg exited with {status}")
    } else {
        format!("ffmpeg exited with {status}: {detail}")
    }
}

fn ffmpeg_log_level() -> &'static str {
    match std::env::var("CRAWLER_MEDIA_FFMPEG_LOG_LEVEL")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "error" => "error",
        "warning" => "warning",
        "info" => "info",
        "verbose" => "verbose",
        "debug" => "debug",
        _ => "warning",
    }
}
