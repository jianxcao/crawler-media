use std::{
    io::Read,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(crate) fn output(
    command: &mut Command,
    deadline_ms: u64,
    control: &crate::WorkControl,
) -> Result<Output, String> {
    output_with_input(command, deadline_ms, control, None, 1_048_576)
}

pub(crate) fn output_with_input(
    command: &mut Command,
    deadline_ms: u64,
    control: &crate::WorkControl,
    input: Option<Vec<u8>>,
    limit: usize,
) -> Result<Output, String> {
    output_with_limits(command, deadline_ms, control, input, limit, 16_384)
}

pub(crate) fn output_with_limits(
    command: &mut Command,
    deadline_ms: u64,
    control: &crate::WorkControl,
    input: Option<Vec<u8>>,
    limit: usize,
    stderr_limit: usize,
) -> Result<Output, String> {
    let mut child = command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("metadata stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("metadata stderr unavailable")?;
    let input_writer = input.zip(child.stdin.take()).map(|(bytes, mut stdin)| {
        thread::spawn(move || {
            use std::io::Write;
            stdin.write_all(&bytes).map_err(|error| error.to_string())
        })
    });
    let out = thread::spawn(move || drain(stdout, limit));
    let err = thread::spawn(move || drain(stderr, stderr_limit));
    let started = Instant::now();
    let mut interrupted = false;
    let status = loop {
        if control.is_cancelled() || started.elapsed() >= Duration::from_millis(deadline_ms) {
            interrupted = true;
            let _ = child.kill();
            break child.wait();
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(e);
            }
        }
    };
    let stdout = out
        .join()
        .map_err(|_| "metadata stdout reader panicked")??;
    let stderr = err
        .join()
        .map_err(|_| "metadata stderr reader panicked")??;
    if let Some(writer) = input_writer {
        let written = writer.join().map_err(|_| "capture input writer panicked")?;
        if !interrupted && status.as_ref().is_ok_and(|status| status.success()) {
            written?;
        }
    }
    if interrupted {
        return Err("metadata capture cancelled or deadline exceeded".into());
    }
    Ok(Output {
        status: status.map_err(|e| e.to_string())?,
        stdout,
        stderr,
    })
}

fn drain(mut reader: impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut result = Vec::new();
    let mut chunk = [0; 4096];
    let mut oversized = false;
    loop {
        let n = reader.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if result.len().saturating_add(n) > limit {
            oversized = true;
        } else {
            result.extend_from_slice(&chunk[..n]);
        }
    }
    if oversized {
        Err("metadata output exceeded bounded capacity".into())
    } else {
        Ok(result)
    }
}
