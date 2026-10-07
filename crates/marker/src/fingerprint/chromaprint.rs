use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use rusty_chromaprint::{Configuration, Fingerprinter};

use crate::target::ProbeTarget;

mod pcm;
mod timing;

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
    let command = build_ffmpeg_command(path, start_secs, duration_secs);
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

fn build_ffmpeg_command(path: &Path, start_secs: u32, duration_secs: u32) -> Command {
    let target = ProbeTarget::from_path(path);
    let mut command = Command::new("ffmpeg");
    command.args(["-v", "error"]);
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
    let stderr_reader = std::thread::spawn(move || read_stderr(stderr));

    let read_result = consume_pcm(stdout, fingerprinter, process_started, timings);
    if read_result.is_err() {
        let _ = child.kill();
    }
    let status = wait_for_ffmpeg(&mut child, timings)?;
    let stderr = collect_stderr(stderr_reader, timings)?;
    read_result?;
    if !status.success() {
        return Err(ffmpeg_exit_error(status, &stderr));
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
    reader: std::thread::JoinHandle<Result<String, String>>,
    timings: &mut ExtractionTimings,
) -> Result<String, String> {
    let started = Instant::now();
    let stderr = reader
        .join()
        .map_err(|_| "ffmpeg stderr reader panicked".to_string())?;
    timings.stderr_collect_ms = started.elapsed().as_millis();
    stderr
}

fn read_stderr(mut stderr: impl Read) -> Result<String, String> {
    let mut output = String::new();
    stderr
        .read_to_string(&mut output)
        .map(|_| output)
        .map_err(|error| error.to_string())
}

fn ffmpeg_exit_error(status: std::process::ExitStatus, stderr: &str) -> String {
    let detail = stderr.trim().chars().take(500).collect::<String>();
    if detail.is_empty() {
        format!("ffmpeg exited with {status}")
    } else {
        format!("ffmpeg exited with {status}: {detail}")
    }
}
