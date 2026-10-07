use marker::{
    AudioFingerprint, ChromaprintEngine, CommonSegment, FingerprintEngine, MarkerType,
    build_complete_timeline_chapters, extract_audio_fingerprint_at_with, find_common_segment,
    match_episodes_fingerprints_with, match_episodes_outros,
};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn word(hash: u32, payload: u32) -> u32 {
    (hash << 20) | (payload & 0x000f_ffff)
}

fn split_three_minute_match() -> (Vec<u32>, Vec<u32>) {
    let mut first = Vec::new();
    let mut second = Vec::new();

    for index in 0..1454 {
        let hash = 500 + index as u32;
        let payload = (index as u32 * 7919 + 31) & 0x000f_ffff;
        first.push(word(hash, payload));
        // The second half remains a valid match, but its different Hamming
        // score makes rusty-chromaprint split one continuous segment in two.
        second.push(word(
            hash,
            if index < 840 {
                payload
            } else {
                payload ^ 0b1111
            },
        ));
    }

    (first, second)
}

#[test]
fn adjacent_matching_ranges_are_combined_into_the_full_three_minute_segment() {
    let (first, second) = split_three_minute_match();
    let found = find_common_segment(&first, &second, 15.0, 240.0)
        .expect("the common voiceprint should be detected");

    assert!(
        (175.0..=185.0).contains(&found.duration_sec),
        "expected the full ~3 minute match, got {:.1}s (start1={:.1}s, start2={:.1}s, score={:.2})",
        found.duration_sec,
        found.start1_sec,
        found.start2_sec,
        found.score
    );
}

#[test]
fn outro_matching_preserves_full_boundaries_and_builds_a_credits_chapter() {
    let (first, second) = split_three_minute_match();
    let detected = match_episodes_outros(&[(1, first, 600_000), (2, second, 900_000)], 15.0, 240.0);

    assert_eq!(detected.len(), 2);
    assert_eq!(detected[0].outro_start_ms, 600_000);
    assert!((175_000..=185_000).contains(&(detected[0].outro_end_ms - detected[0].outro_start_ms)));

    let chapters = build_complete_timeline_chapters(
        &[],
        None,
        Some((detected[0].outro_start_ms, detected[0].outro_end_ms)),
        Some(1_200_000),
    );
    assert!(chapters.iter().any(|chapter| {
        chapter.marker_type == Some(MarkerType::CreditsStart)
            && chapter.start_ms == detected[0].outro_start_ms
            && chapter.end_ms == detected[0].outro_end_ms
    }));
}

struct FixedEngine;

impl FingerprintEngine for FixedEngine {
    fn extract_at(
        &self,
        _path: &Path,
        start_secs: u32,
        duration_secs: u32,
    ) -> Result<AudioFingerprint, String> {
        Ok(vec![start_secs, duration_secs])
    }

    fn find_common_segment(
        &self,
        _first: &[u32],
        _second: &[u32],
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Option<CommonSegment> {
        Some(CommonSegment {
            start1_sec: 2.0,
            end1_sec: 182.0,
            start2_sec: 5.0,
            end2_sec: 185.0,
            duration_sec: 180.0,
            score: 0.5,
        })
    }
}

#[test]
fn a_different_fingerprint_engine_can_be_injected_without_ffmpeg() {
    let engine = FixedEngine;
    let fingerprint = extract_audio_fingerprint_at_with(&engine, Path::new("ignored.mkv"), 12, 30)
        .expect("injected extraction should be used");
    assert_eq!(fingerprint, vec![12, 30]);

    let matched =
        match_episodes_fingerprints_with(&engine, &[(1, vec![1]), (2, vec![2])], 15.0, 240.0);
    assert_eq!(matched.len(), 2);
    assert_eq!(matched[0].intro_start_ms, 2_000);
    assert_eq!(matched[1].intro_start_ms, 5_000);
}

struct TruncatedRemoteStreamOnce {
    attempts: AtomicUsize,
}

impl FingerprintEngine for TruncatedRemoteStreamOnce {
    fn extract_at(
        &self,
        _path: &Path,
        _start_secs: u32,
        _duration_secs: u32,
    ) -> Result<AudioFingerprint, String> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("Empty fingerprint extracted: File ended prematurely".into())
        } else {
            Ok(vec![7, 11, 13])
        }
    }

    fn find_common_segment(
        &self,
        _first: &[u32],
        _second: &[u32],
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Option<CommonSegment> {
        None
    }
}

#[test]
fn remote_truncated_fingerprint_sample_is_retried_once() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("episode.strm");
    std::fs::write(&path, "https://media.example/episode.mkv\n").unwrap();
    let engine = TruncatedRemoteStreamOnce {
        attempts: AtomicUsize::new(0),
    };

    let fingerprint =
        extract_audio_fingerprint_at_with(&engine, &path, 2398, 180).expect("retry should recover");

    assert_eq!(fingerprint, vec![7, 11, 13]);
    assert_eq!(engine.attempts.load(Ordering::SeqCst), 2);
}

struct TransientRemoteStreamTwice {
    attempts: AtomicUsize,
    error: &'static str,
}

impl FingerprintEngine for TransientRemoteStreamTwice {
    fn extract_at(
        &self,
        _path: &Path,
        _start_secs: u32,
        _duration_secs: u32,
    ) -> Result<AudioFingerprint, String> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) < 2 {
            Err(self.error.into())
        } else {
            Ok(vec![17, 19, 23])
        }
    }

    fn find_common_segment(
        &self,
        _first: &[u32],
        _second: &[u32],
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Option<CommonSegment> {
        None
    }
}

#[test]
fn remote_truncated_fingerprint_retries_after_repeated_transient_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("episode.strm");
    std::fs::write(&path, "https://media.example/episode.mkv\n").unwrap();
    let engine = TransientRemoteStreamTwice {
        attempts: AtomicUsize::new(0),
        error: "Empty fingerprint extracted: File ended prematurely",
    };

    let fingerprint = extract_audio_fingerprint_at_with(&engine, &path, 2398, 180)
        .expect("retries should recover after repeated temporary truncation");

    assert_eq!(fingerprint, vec![17, 19, 23]);
    assert_eq!(engine.attempts.load(Ordering::SeqCst), 3);
}

#[test]
fn remote_read_errors_are_retried_after_transient_truncated_streams() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("episode.strm");
    std::fs::write(&path, "https://media.example/episode.mkv\n").unwrap();
    let engine = TransientRemoteStreamTwice {
        attempts: AtomicUsize::new(0),
        error: "Empty fingerprint extracted: Read error",
    };

    let fingerprint = extract_audio_fingerprint_at_with(&engine, &path, 2398, 180)
        .expect("remote read errors should retry and recover");

    assert_eq!(fingerprint, vec![17, 19, 23]);
    assert_eq!(engine.attempts.load(Ordering::SeqCst), 3);
}

#[cfg(unix)]
#[test]
fn chromaprint_engine_logs_extraction_phase_timings() {
    const CHILD_FLAG: &str = "MARKER_TIMING_TEST_CHILD";
    const PCM_PATH: &str = "MARKER_TIMING_TEST_PCM_PATH";

    if std::env::var_os(CHILD_FLAG).is_some() {
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .try_init();
        let fingerprint = ChromaprintEngine
            .extract_at(Path::new("ignored.mkv"), 0, 30)
            .expect("fake ffmpeg should return decodable PCM");
        assert!(!fingerprint.is_empty());
        return;
    }

    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let pcm_path = temp.path().join("audio.pcm");
    let mut pcm = Vec::with_capacity(30 * 16_000 * 2);
    for sample_index in 0..(30 * 16_000) {
        let sample = (f32::sin(2.0 * std::f32::consts::PI * 440.0 * sample_index as f32 / 16_000.0)
            * 10_000.0) as i16;
        pcm.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(&pcm_path, pcm).expect("PCM fixture should be written");

    let bin_dir = temp.path().join("bin");
    std::fs::create_dir(&bin_dir).expect("fake ffmpeg directory should be created");
    let ffmpeg = bin_dir.join("ffmpeg");
    std::fs::write(
        &ffmpeg,
        "#!/bin/sh\nexec /bin/cat \"$MARKER_TIMING_TEST_PCM_PATH\"\n",
    )
    .expect("fake ffmpeg should be written");
    let mut permissions = std::fs::metadata(&ffmpeg)
        .expect("fake ffmpeg metadata should be readable")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&ffmpeg, permissions).expect("fake ffmpeg should be executable");

    let output = Command::new(std::env::current_exe().expect("test executable path should exist"))
        .args([
            "--exact",
            "chromaprint_engine_logs_extraction_phase_timings",
            "--nocapture",
        ])
        .env(CHILD_FLAG, "1")
        .env(PCM_PATH, &pcm_path)
        .env("PATH", &bin_dir)
        .output()
        .expect("child test process should run");
    let logs = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "child test failed: {logs}");
    for field in [
        "ffmpeg_spawn_ms=",
        "time_to_first_pcm_ms=",
        "pcm_read_wait_us=",
        "chromaprint_consume_us=",
        "chromaprint_finish_ms=",
        "ffmpeg_wait_ms=",
        "pcm_bytes=",
        "sample_count=",
    ] {
        assert!(logs.contains(field), "missing {field} in logs: {logs}");
    }
}
