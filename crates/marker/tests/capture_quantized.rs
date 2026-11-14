use marker::{CaptureRequest, FingerprintCaptureEngine, PcmTimeline, SampleWindow};
use std::{path::Path, process::Command};
#[path = "capture_quantized/aac48.rs"]
mod aac48;
#[path = "capture_quantized/container.rs"]
mod container;
#[path = "capture_quantized/decisions.rs"]
mod decisions;
#[path = "capture_quantized/head.rs"]
mod head;
#[path = "capture_quantized/unusable.rs"]
mod unusable;

fn ff(args: &[&str], output: &Path) {
    let result = Command::new("ffmpeg")
        .args([
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

#[test]
fn same_aac_payload_quantized_phase_preserves_pcm_and_proves_bounded_mapping() {
    same_payload_phase(44_100);
}

fn same_payload_phase(rate: u32) {
    let dir = tempfile::tempdir().unwrap();
    let mp4 = dir.path().join("source.mp4");
    ff(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:r=1:d=12",
            "-f",
            "lavfi",
            "-i",
            &format!("anoisesrc=seed=42:sample_rate={rate}:duration=12"),
            "-ac",
            "2",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
            "-threads",
            "1",
        ],
        &mp4,
    );
    let mkv = dir.path().join("phase.mkv");
    ff(
        &[
            "-ss",
            &format!("{:.6}", 1024. / f64::from(rate)),
            "-i",
            mp4.to_str().unwrap(),
            "-c",
            "copy",
        ],
        &mkv,
    );
    assert_eq!(
        audio_payload_hashes(&mp4),
        audio_payload_hashes(&mkv),
        "the phase-only fixture must retain every encoded access unit"
    );
    let reference = capture(&mp4, 0, 10_000);
    let quantized = capture(&mkv, 0, 10_000);
    assert_eq!(
        quantized.metrics.pcm_timeline,
        PcmTimeline::Contiguous,
        "{:?}",
        quantized.metrics.pcm_timeline
    );
    assert_eq!(quantized.metrics.sample_count, Some(160_000));
    assert_eq!(quantized.metrics.pcm_sha256, reference.metrics.pcm_sha256);
    assert_eq!(quantized.metrics.time_mapping_version, 4);
    assert!(
        !marker::boundary::evidence::complete_pcm(&quantized),
        "finite source precision cannot prove an exact capture edge"
    );
    if rate == 48_000 {
        assert!(
            !marker::boundary::evidence::complete_guards(
                &quantized,
                marker::boundary::Interval {
                    start_ms: 3_001,
                    end_ms: 6_999
                },
                marker::SegmentKind::Intro,
                None,
                &marker::boundary::BoundaryPolicy::default(),
            ),
            "a 1334us origin bound cannot prove a 1ms left guard"
        );
    }
    assert!(
        marker::boundary::evidence::complete_guards(
            &quantized,
            marker::boundary::Interval {
                start_ms: if rate == 48_000 { 3_002 } else { 3_001 },
                end_ms: 6_999
            },
            marker::SegmentKind::Intro,
            None,
            &marker::boundary::BoundaryPolicy::default()
        ),
        "mapping={:?}, coverage={:?}",
        quantized.metrics.quantized_mapping,
        marker::boundary::evidence::candidate_pcm_coverage(&quantized)
    );
}

fn audio_payload_hashes(path: &Path) -> Vec<String> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_packets",
            "-show_data_hash",
            "sha256",
            "-show_entries",
            "packet=data_hash",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    value["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|packet| packet["data_hash"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn measured_variable_laces_accept_floor_and_nearest_without_guessing_groups() {
    let dir = tempfile::tempdir().unwrap();
    let payloads = container::payloads(dir.path());
    let mut previous = None;
    for floor in [false, true] {
        let file = dir.path().join(format!("laced-{floor}.mka"));
        container::write(&file, &payloads, floor, 0, |_| 0);
        let captured = capture(&file, 0, 10_000);
        assert_eq!(captured.metrics.pcm_timeline, PcmTimeline::Contiguous);
        let proof = captured.metrics.quantized_mapping.as_ref().unwrap();
        assert!(proof.block_anchors > 10);
        assert!(proof.block_anchors * 3 < proof.packet_count);
        assert!(
            proof.origin_min_us <= 0 && proof.origin_max_us >= 0,
            "{proof:?}"
        );
        assert!(captured.metrics.contiguous_pcm_start_ms.is_none());
        assert!(!marker::boundary::evidence::complete_pcm(&captured));
        if let Some(hash) = previous {
            assert_eq!(captured.metrics.pcm_sha256, hash);
        }
        previous = Some(captured.metrics.pcm_sha256.clone());
        let candidate = marker::boundary::normalize_capture_interval(
            &captured,
            marker::boundary::Interval {
                start_ms: 3_000,
                end_ms: 7_000,
            },
        )
        .unwrap();
        assert!(candidate.start_ms >= 3_000 && candidate.end_ms <= 7_000);
        assert!(
            !marker::boundary::evidence::complete_guards(
                &captured,
                marker::boundary::Interval {
                    start_ms: 5_000,
                    end_ms: 10_000
                },
                marker::SegmentKind::Outro,
                Some(10_000),
                &marker::boundary::BoundaryPolicy::default()
            ),
            "quantized right edge cannot borrow exact media end"
        );
    }
}

#[test]
fn grouped_source_gap_overlap_and_cumulative_drift_cannot_be_normalized_away() {
    let dir = tempfile::tempdir().unwrap();
    let payloads = container::payloads(dir.path());
    for (name, direction) in [("gap", 1), ("overlap", -1), ("drift", 0)] {
        let file = dir.path().join(format!("{name}.mka"));
        container::write(&file, &payloads, false, 0, |group| {
            if direction == 0 {
                group as i64 / 3
            } else if group >= 12 {
                direction * 8
            } else {
                0
            }
        });
        let result = marker::ChromaprintEngine.capture_window(&CaptureRequest {
            path: file,
            window: SampleWindow::new(0, 10_000),
            audio_stream_index: None,
            process_deadline_ms: 30_000,
        });
        let error = result.expect_err("one global clock phase cannot admit changed block anchors");
        assert!(
            error.message.contains("source_block_clock_discontinuity"),
            "{name}: {error}"
        );
    }
}

#[test]
fn interior_laced_capture_has_measured_preroll_and_honest_origin_range() {
    let dir = tempfile::tempdir().unwrap();
    let payloads = container::payloads(dir.path());
    let file = dir.path().join("interior.mka");
    container::write(&file, &payloads, true, 0, |_| 0);
    let captured = capture(&file, 1_013, 10_027);
    assert_eq!(captured.metrics.pcm_timeline, PcmTimeline::Contiguous);
    let proof = captured.metrics.quantized_mapping.unwrap();
    assert!(
        proof.origin_min_us <= 1_013_000 && proof.origin_max_us >= 1_012_000,
        "{proof:?}"
    );
    assert!(proof.origin_max_us - proof.origin_min_us <= 2001);
    assert_eq!(captured.window, SampleWindow::new(1_013, 10_027));
    assert_eq!(captured.metrics.sample_count, Some(144_224));
}
