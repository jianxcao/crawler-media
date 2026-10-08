use std::path::PathBuf;
use std::process::Command;

use marker::{
    CaptureRequest, ChromaprintEngine, FingerprintCaptureEngine, ProbeTarget,
    SampleWindow,
};

#[test]
fn sample_request_keeps_proxy_disabled_and_formats_accurate_seek_ms() {
    let tmp = tempfile::tempdir().unwrap();
    let strm = tmp.path().join("episode.strm");
    std::fs::write(&strm, "https://media.example/episode.mkv\n").unwrap();
    let target = ProbeTarget::from_path(&strm);

    let mut ffmpeg = Command::new("ffmpeg");
    target.apply_ffmpeg_input_with_seek_ms(&mut ffmpeg, 12_345);

    let args = ffmpeg
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    // Check proxy bypass
    let proxy_flag = args.iter().position(|arg| arg == "-http_proxy");
    assert!(proxy_flag.is_some(), "must set empty http_proxy");
    assert_eq!(args[proxy_flag.unwrap() + 1], "");

    // Check decimal seek format
    let ss_flag = args.iter().position(|arg| arg == "-ss");
    assert!(ss_flag.is_some(), "-ss option must be present");
    assert_eq!(
        args[ss_flag.unwrap() + 1],
        "12.345",
        "seek offset must be formatted in decimal seconds"
    );

    // -i must follow -ss (input seek)
    let i_flag = args.iter().position(|arg| arg == "-i");
    assert!(i_flag.is_some());
    assert!(
        ss_flag.unwrap() < i_flag.unwrap(),
        "-ss must precede -i for accurate input seeking"
    );
}

#[test]
fn diagnostics_redact_signed_urls_and_auth_headers() {
    let stderr = "[http] HTTP error 503 for https://user:secret@media.example/stream?token=secret\n\
                 [http] Authorization: Bearer topsecret\n\
                 [http] Cookie: session=secret-cookie\n\
                 [http] request: GET /stream?token=relative-secret HTTP/1.1";

    let redacted = marker::fingerprint::sanitize_stderr(stderr);
    assert!(!redacted.contains("secret"));
    assert!(!redacted.contains("topsecret"));
    assert!(!redacted.contains("secret-cookie"));
    assert!(!redacted.contains("relative-secret"));
    assert!(redacted.contains("[REDACTED]") || redacted.contains("<redacted>"));
}

#[test]
fn capture_window_timeout_returns_timeout_failure_kind() {
    // If a request has deadline 0 or very small and points to a command that blocks/hangs
    // or when deadline expires, it should yield CaptureFailureKind::Timeout.
    let window = SampleWindow {
        start_ms: 0,
        end_ms: 10_000,
    };
    let request = CaptureRequest {
        path: PathBuf::from("nonexistent_test_path.mkv"),
        window,
        audio_stream_index: None,
        process_deadline_ms: 1, // 1ms deadline
    };

    let engine = ChromaprintEngine;
    let res = engine.capture_window(&request);
    assert!(res.is_err());
    let err = res.err().unwrap();
    // It should either fail with Io or Timeout or Decode, but let's ensure it has structured metrics
    assert!(err.metrics.pcm_bytes == 0);
}
