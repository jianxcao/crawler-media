use marker::{CaptureRequest, FingerprintCaptureEngine, PcmTimeline, SampleWindow};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn ff(args: &[&str], output: &Path) {
    let result = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-nostdin",
            "-y",
            "-filter_threads",
            "1",
            "-threads",
            "1",
        ])
        .args(args)
        .arg(output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
fn source(root: &Path, rate: u32, seconds: u32) -> PathBuf {
    let file = root.join(format!("source-{rate}.wav"));
    ff(
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("anoisesrc=seed=42:sample_rate={rate}:duration={seconds}"),
            "-c:a",
            "pcm_s16le",
            "-threads",
            "1",
        ],
        &file,
    );
    file
}
fn capture(path: &Path, start: i64, end: i64) -> marker::CapturedFingerprint {
    marker::ChromaprintEngine
        .capture_window(&CaptureRequest {
            path: path.into(),
            window: SampleWindow::new(start, end),
            audio_stream_index: None,
            process_deadline_ms: 30_000,
        })
        .unwrap()
}
fn independent_pcm(path: &Path, start: &str, end: &str) -> Vec<u8> {
    // Exact same-container legacy seek/decode is the resampling waveform oracle;
    // full-file resampling has a different phase/history and is not equivalent.
    let duration = end.parse::<f64>().unwrap() - start.parse::<f64>().unwrap();
    let result = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-threads",
            "1",
            "-accurate_seek",
            "-ss",
            start,
            "-i",
        ])
        .arg(path)
        .args([
            "-t",
            &duration.to_string(),
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-sample_fmt",
            "s16",
            "-c:a",
            "pcm_s16le",
            "-threads",
            "1",
            "-f",
            "s16le",
            "-",
        ])
        .output()
        .unwrap();
    assert!(result.status.success());
    result.stdout
}
fn matches_pcm(captured: &marker::CapturedFingerprint, expected: &[u8]) {
    assert_eq!(
        captured.metrics.sample_count,
        Some(expected.len() as u64 / 2)
    );
    assert_eq!(
        captured.metrics.pcm_sha256.as_deref(),
        Some(format!("{:x}", Sha256::digest(expected)).as_str())
    );
}

#[test]
fn resampling_preserves_waveform_partial_eof_and_observed_origin() {
    let root = tempfile::tempdir().unwrap();
    let wav = source(root.path(), 44_100, 12);
    let input = root.path().join("source.nut");
    ff(&["-i", wav.to_str().unwrap(), "-c:a", "copy"], &input);
    let captured = capture(&input, 1_013, 10_027);
    matches_pcm(&captured, &independent_pcm(&input, "1.013", "10.027"));
    assert_eq!(captured.metrics.pcm_timeline, PcmTimeline::Contiguous);
    assert_eq!(captured.metrics.first_decoded_source_pts, Some(16_208));
    assert_eq!(captured.metrics.contiguous_pcm_start_ms, Some(1_013));
    let eof = capture(&input, 8_000, 18_000);
    matches_pcm(&eof, &independent_pcm(&input, "8", "12"));
    assert_eq!(eof.metrics.contiguous_pcm_start_ms, Some(8_000));
    assert!(!marker::boundary::evidence::complete_pcm(&eof));
    assert_eq!(
        marker::boundary::evidence::candidate_pcm_coverage(&eof)
            .unwrap()
            .end_ms,
        12_000
    );
}

#[test]
fn complete_length_cannot_hide_gap_or_overlap() {
    let root = tempfile::tempdir().unwrap();
    let input = source(root.path(), 16_000, 12);
    for (name, sign) in [("gap", "+"), ("overlap", "-")] {
        let file = root.path().join(format!("{name}.mkv"));
        ff(
            &[
                "-i",
                input.to_str().unwrap(),
                "-af",
                &format!("asetnsamples=n=256:p=0,asetpts=PTS{sign}if(gte(N\\,16000)\\,256\\,0)"),
                "-c:a",
                "pcm_s16le",
                "-threads",
                "1",
            ],
            &file,
        );
        let captured = capture(&file, 0, 10_000);
        assert!(captured.pcm_duration_ms.unwrap() >= 9_980);
        assert!(
            matches!(captured.metrics.pcm_timeline, PcmTimeline::Invalid(_)),
            "{name}: {:?}",
            captured.metrics
        );
        assert!(captured.metrics.contiguous_pcm_start_ms.is_none());
        assert!(!marker::boundary::evidence::complete_pcm(&captured));
        assert!(marker::boundary::evidence::candidate_pcm_coverage(&captured).is_none());
    }
}

#[test]
fn aac_zero_seek_omission_preserves_nonzero_origin_and_long_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    let input = source(root.path(), 48_000, 190);
    let aac = root.path().join("aac.mp4");
    ff(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:r=1:d=190",
            "-i",
            input.to_str().unwrap(),
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
            "-threads",
            "1",
        ],
        &aac,
    );
    let shifted = root.path().join("shifted.mkv");
    ff(
        &[
            "-copyts",
            "-itsoffset",
            "5",
            "-i",
            aac.to_str().unwrap(),
            "-c:a",
            "copy",
            "-avoid_negative_ts",
            "disabled",
        ],
        &shifted,
    );
    let original = capture(&aac, 0, 180_000);
    let shifted_capture = capture(&shifted, 0, 180_000);
    assert_eq!(
        shifted_capture.metrics.pcm_sha256,
        original.metrics.pcm_sha256
    );
    assert_eq!(shifted_capture.metrics.sample_count, Some(2_880_000));
    assert_eq!(
        shifted_capture.metrics.first_decoded_source_pts,
        Some(80_000)
    );
    assert_eq!(shifted_capture.metrics.contiguous_pcm_start_ms, Some(0));
    assert_eq!(
        shifted_capture.metrics.pcm_timeline,
        PcmTimeline::Contiguous
    );
    assert!(shifted_capture.metrics.ffmpeg_stderr_bytes.unwrap() > 16_384);
    let interior = capture(&aac, 47_013, 53_027);
    matches_pcm(&interior, &independent_pcm(&aac, "47.013", "53.027"));
    assert_eq!(interior.metrics.contiguous_pcm_start_ms, Some(47_013));
    assert!(interior.metrics.capture_process_ms.unwrap() < 30_000);
}
