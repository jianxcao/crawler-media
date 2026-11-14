use marker::boundary::{Interval, normalize_interval};
use marker::{CaptureRequest, FingerprintCaptureEngine, FingerprintEngine, SampleWindow};

fn audio(seed: u64, seconds: usize) -> Vec<i16> {
    let mut state = seed;
    let mut frequencies = [0.0; 4];
    (0..seconds * 16_000)
        .map(|i| {
            if i % 3200 == 0 {
                for frequency in &mut frequencies {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    *frequency = 120.0 + ((state >> 32) % 2400) as f64;
                }
            }
            (frequencies
                .iter()
                .map(|frequency| (i as f64 * frequency * std::f64::consts::TAU / 16_000.0).sin())
                .sum::<f64>()
                * 5000.0) as i16
        })
        .collect()
}
fn write_wav(path: &std::path::Path, samples: &[i16]) {
    let length = samples.len() as u32 * 2;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend((length + 36).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(16_000u32.to_le_bytes());
    bytes.extend(32_000u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend(length.to_le_bytes());
    for sample in samples {
        bytes.extend(sample.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}
fn capture_fixtures(
    root: &std::path::Path,
) -> Vec<(marker::CapturedFingerprint, marker::CapturedFingerprint)> {
    let intro = audio(100, 40);
    let outro = audio(200, 40);
    let mut captures = Vec::new();
    for (episode, shift) in [(1, 0), (2, 0), (3, 17)] {
        let mut pcm = audio(episode, 240 + shift);
        pcm[(30 + shift) * 16_000..(70 + shift) * 16_000].copy_from_slice(&intro);
        pcm[(170 + shift) * 16_000..(210 + shift) * 16_000].copy_from_slice(&outro);
        // Fixed silence and a dialogue-like overlay outside the shared regions.
        pcm[0..16_000].fill(0);
        for (index, sample) in pcm[(90 + shift) * 16_000..(100 + shift) * 16_000]
            .iter_mut()
            .enumerate()
        {
            *sample = sample.saturating_add(((index as f64 * 0.4).sin() * 3000.0) as i16);
        }
        let path = root.join(format!("episode-{episode}.wav"));
        write_wav(&path, &pcm);
        let capture = |start, end| {
            marker::ChromaprintEngine
                .capture_window(&CaptureRequest {
                    path: path.clone(),
                    window: SampleWindow::new(start, end),
                    audio_stream_index: None,
                    process_deadline_ms: 30_000,
                })
                .unwrap()
        };
        captures.push((
            capture(0, (120 + shift) as i64 * 1000),
            capture((120 + shift) as i64 * 1000, (240 + shift) as i64 * 1000),
        ));
    }
    captures
}

#[test]
fn real_ffmpeg_capture_calibrates_four_endpoints_and_seventeen_second_shift() {
    assert!(
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok(),
        "FFmpeg required: calibration not run"
    );
    let temp = tempfile::tempdir().unwrap();
    let captures = capture_fixtures(temp.path());
    let mut measured = Vec::new();
    for (index, shift) in [(1, 0), (2, 17_000)] {
        for (kind, expected) in [
            (
                0,
                Interval {
                    start_ms: 30_000 + shift,
                    end_ms: 70_000 + shift,
                },
            ),
            (
                1,
                Interval {
                    start_ms: 170_000 + shift,
                    end_ms: 210_000 + shift,
                },
            ),
        ] {
            let first = if kind == 0 {
                &captures[index].0
            } else {
                &captures[index].1
            };
            let reference = if kind == 0 {
                &captures[0].0
            } else {
                &captures[0].1
            };
            let segment = marker::ChromaprintEngine
                .find_common_segment(&first.words, &reference.words, 15.0, 60.0)
                .expect("known shared audio must match");
            let interval = normalize_interval(
                &first.window,
                Interval {
                    start_ms: (segment.start1_sec * 1000.0).round() as i64,
                    end_ms: (segment.end1_sec * 1000.0).round() as i64,
                },
            )
            .unwrap();
            eprintln!(
                "calibration kind={kind} shift={shift}: start_error_ms={} end_error_ms={}",
                interval.start_ms - expected.start_ms,
                interval.end_ms - expected.end_ms
            );
            measured.push(interval);
            // Raw fingerprint coordinates have configuration-dependent feature support;
            // these independent source boundaries must not be promoted into the candidate.
            let kind = if kind == 0 {
                marker::SegmentKind::Intro
            } else {
                marker::SegmentKind::Outro
            };
            let evidence = marker::boundary::evidence::collect_boundary_evidence(
                first,
                interval,
                &[expected],
                kind,
                Some(240_000 + shift),
                marker::boundary::evidence::ReferenceSupport {
                    references_agree: true,
                    complete_guards: true,
                },
                &marker::boundary::BoundaryPolicy::default(),
            );
            if interval.start_ms.abs_diff(expected.start_ms) > 1000
                || interval.end_ms.abs_diff(expected.end_ms) > 1000
            {
                assert!(
                    matches!(
                        marker::boundary::refine_boundary(
                            &evidence,
                            &marker::boundary::BoundaryPolicy::default()
                        ),
                        marker::boundary::BoundaryDecision::Uncertain(_)
                    ),
                    "known inaccurate raw endpoints must never publish"
                );
            }
        }
    }
    for (original, shifted) in measured[..2].iter().zip(&measured[2..]) {
        assert!(
            (shifted.start_ms - original.start_ms).abs_diff(17_000) <= 1000,
            "start must track physical shift"
        );
        assert!(
            (shifted.end_ms - original.end_ms).abs_diff(17_000) <= 1000,
            "end must track physical shift"
        );
    }
}

fn multitrack(
    root: &std::path::Path,
    first: &std::path::Path,
    second: &std::path::Path,
    first_pts: i64,
    second_pts: i64,
) -> std::path::PathBuf {
    let output = root.join(format!("tracks-{first_pts}-{second_pts}.mkv"));
    let result = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-itsoffset",
            &first_pts.to_string(),
            "-i",
        ])
        .arg(first)
        .args(["-itsoffset", &second_pts.to_string(), "-i"])
        .arg(second)
        .args(["-map", "0:a:0", "-map", "1:a:0", "-c:a", "pcm_s16le"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=index,start_time",
            "-of",
            "json",
        ])
        .arg(&output)
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    assert_eq!(
        json["streams"][0]["start_time"],
        format!("{first_pts}.000000")
    );
    assert_eq!(
        json["streams"][1]["start_time"],
        format!("{second_pts}.000000")
    );
    output
}
#[test]
fn nonzero_container_pts_and_explicit_audio_selection_preserve_media_relative_time() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first.wav");
    let second = root.path().join("second.wav");
    write_wav(&first, &audio(11, 90));
    write_wav(&second, &audio(22, 90));
    let common_pts = multitrack(root.path(), &first, &second, 5, 5);
    let mixed_pts = multitrack(root.path(), &first, &second, 0, 5);
    let capture = |path: &std::path::Path, start, end, index| {
        marker::ChromaprintEngine
            .capture_window(&CaptureRequest {
                path: path.to_path_buf(),
                window: SampleWindow::new(start, end),
                audio_stream_index: index,
                process_deadline_ms: 30_000,
            })
            .unwrap()
    };
    let baseline = capture(&second, 10_000, 50_000, None);
    let selected = capture(&common_pts, 10_000, 50_000, Some(1));
    assert_eq!(
        selected.words, baseline.words,
        "container starts at PTS5s; seek remains media-relative10s"
    );
    assert_eq!(selected.pcm_duration_ms, Some(40_000));
    assert_eq!(selected.metrics.pcm_rms_dbfs_milli.len(), 2_000);
    assert_eq!(selected.metrics.audio_stream_index, Some(1));
    assert_eq!(selected.metrics.contiguous_pcm_start_ms, Some(10_000));
    assert_eq!(
        selected.metrics.source_start_pts_ms, None,
        "unknown source PTS must remain unknown"
    );
    assert_eq!(
        normalize_interval(
            &selected.window,
            Interval {
                start_ms: 5_000,
                end_ms: 20_000
            }
        )
        .unwrap(),
        Interval {
            start_ms: 15_000,
            end_ms: 30_000
        }
    );
    let other = capture(&common_pts, 10_000, 50_000, Some(0));
    assert_ne!(
        selected.words, other.words,
        "explicit stream index must select a different waveform"
    );
    let delayed = capture(&mixed_pts, 10_000, 50_000, Some(1));
    assert_eq!(
        delayed.words,
        capture(&second, 5_000, 45_000, None).words,
        "selected audio delay is relative to format origin"
    );
    let clipped = capture(&mixed_pts, 0, 40_000, Some(1));
    assert_eq!(clipped.metrics.contiguous_pcm_start_ms, Some(5_000));
    assert!(
        !marker::boundary::evidence::complete_pcm(&clipped),
        "missing first5s cannot masquerade as a complete window"
    );
    assert!(
        marker::boundary::evidence::candidate_pcm_coverage(&clipped).is_none(),
        "unknown leading alignment cannot become proven suffix loss even when total PCM could cover an interior candidate"
    );
}
