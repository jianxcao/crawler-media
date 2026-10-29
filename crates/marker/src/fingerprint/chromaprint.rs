use rusty_chromaprint::{Configuration, Fingerprinter};

pub(crate) mod diagnostics;
pub(crate) mod timing;

pub(crate) use diagnostics::{CapturedStderr, sanitized_excerpt};
pub(crate) use timing::{ExtractionTimings, log_extraction_timings};

pub(crate) fn start_fingerprinter() -> Result<Fingerprinter, String> {
    let config = Configuration::preset_test2();
    let mut fingerprinter = Fingerprinter::new(&config);
    fingerprinter
        .start(16000, 1)
        .map_err(|error| format!("fingerprinter start failed: {error:?}"))?;
    Ok(fingerprinter)
}

pub(crate) fn ffmpeg_exit_error(status: std::process::ExitStatus, stderr: &str) -> String {
    let detail = sanitized_excerpt(stderr);
    if detail.is_empty() {
        format!("ffmpeg exited with {status}")
    } else {
        format!("ffmpeg exited with {status}: {detail}")
    }
}

pub(crate) fn ffmpeg_log_level() -> &'static str {
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
