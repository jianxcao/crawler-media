use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use rusty_chromaprint::{Configuration, Fingerprinter};

use crate::target::ProbeTarget;

pub(super) fn extract(
    path: &Path,
    start_secs: u32,
    duration_secs: u32,
) -> Result<Vec<u32>, String> {
    let config = Configuration::preset_test2();
    let mut fingerprinter = Fingerprinter::new(&config);
    fingerprinter
        .start(16000, 1)
        .map_err(|error| format!("fingerprinter start failed: {error:?}"))?;

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

    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Failed to capture ffmpeg stdout".into());
    };
    let Some(mut stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Failed to capture ffmpeg stderr".into());
    };
    let stderr_reader = std::thread::spawn(move || {
        let mut output = String::new();
        stderr
            .read_to_string(&mut output)
            .map(|_| output)
            .map_err(|error| error.to_string())
    });

    let read_result = consume_pcm(stdout, &mut fingerprinter);
    if read_result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|error| error.to_string())?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "ffmpeg stderr reader panicked".to_string())??;
    read_result?;

    if !status.success() {
        let detail = stderr.trim().chars().take(500).collect::<String>();
        return Err(if detail.is_empty() {
            format!("ffmpeg exited with {status}")
        } else {
            format!("ffmpeg exited with {status}: {detail}")
        });
    }

    fingerprinter.finish();
    let fingerprint = fingerprinter.fingerprint().to_vec();
    if fingerprint.is_empty() {
        let detail = stderr.trim().chars().take(500).collect::<String>();
        return Err(if detail.is_empty() {
            "Empty fingerprint extracted (ffmpeg produced no audio samples)".into()
        } else {
            format!("Empty fingerprint extracted: {detail}")
        });
    }
    Ok(fingerprint)
}

fn consume_pcm(mut stdout: impl Read, fingerprinter: &mut Fingerprinter) -> Result<(), String> {
    let mut bytes = [0u8; 4096];
    let mut samples = Vec::with_capacity(bytes.len() / 2 + 1);
    let mut pending_low_byte = None;

    loop {
        let length = stdout.read(&mut bytes).map_err(|error| error.to_string())?;
        if length == 0 {
            break;
        }
        samples.clear();
        let mut start = 0;
        if let Some(low_byte) = pending_low_byte.take() {
            samples.push(i16::from_le_bytes([low_byte, bytes[0]]));
            start = 1;
        }
        let aligned_length = (length - start) & !1;
        for sample in bytes[start..start + aligned_length].chunks_exact(2) {
            samples.push(i16::from_le_bytes([sample[0], sample[1]]));
        }
        if start + aligned_length < length {
            pending_low_byte = Some(bytes[length - 1]);
        }
        fingerprinter.consume(&samples);
    }

    if pending_low_byte.is_some() {
        return Err("ffmpeg produced an incomplete 16-bit PCM sample".into());
    }
    Ok(())
}
