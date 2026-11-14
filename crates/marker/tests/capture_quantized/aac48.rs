use super::*;
use sha2::{Digest, Sha256};

#[test]
fn same_aac48_payload_quantized_phase_preserves_pcm() {
    same_payload_phase(48_000);
}

fn payload_pcm(path: &Path, samples: usize) -> Vec<u8> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(path)
        .args([
            "-af",
            &format!("aresample=16000:async=0,aformat=sample_fmts=s16:channel_layouts=mono,atrim=end_sample={samples}"),
            "-c:a", "pcm_s16le", "-threads", "1", "-f", "s16le", "pipe:1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn variable_laced_aac48_floor_and_nearest_preserve_payload_pcm_and_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let payloads = container::payloads_at_rate(dir.path(), 12, 48_000);
    let encoded = dir.path().join("encoded.aac");
    let expected = payload_pcm(&encoded, 160_000);
    assert_eq!(expected.len(), 320_000);
    for floor in [false, true] {
        let file = dir.path().join(format!("aac48-{floor}.mka"));
        container::write_at_rate(&file, &payloads, floor, 0, |_| 0, 48_000);
        assert_eq!(
            audio_payload_hashes(&file),
            payloads
                .iter()
                .map(|bytes| format!("SHA256:{:x}", Sha256::digest(bytes)))
                .collect::<Vec<_>>()
        );
        let captured = capture(&file, 0, 10_000);
        assert_eq!(captured.metrics.pcm_timeline, PcmTimeline::Contiguous);
        assert_eq!(captured.metrics.sample_count, Some(160_000));
        assert_eq!(
            captured.metrics.pcm_sha256,
            Some(format!("{:x}", Sha256::digest(&expected)))
        );
        let proof = captured.metrics.quantized_mapping.as_ref().unwrap();
        assert_eq!(proof.native_sample_rate, 48_000);
        assert!(proof.block_anchors > 10 && proof.block_anchors * 3 < proof.packet_count);
        assert!(
            proof.origin_min_us <= 0 && proof.origin_max_us >= 0,
            "{proof:?}"
        );
        assert_eq!(proof.codec_native_samples, proof.packet_count * 1024);
        assert!(proof.decoded_native_samples >= 480_000);
        assert!(proof.decoded_native_samples <= proof.codec_native_samples);
        assert_eq!(captured.metrics.time_mapping_version, 4);
        assert!(captured.metrics.contiguous_pcm_start_ms.is_none());
        assert!(!marker::boundary::evidence::complete_pcm(&captured));
        let candidate = marker::boundary::normalize_capture_interval(
            &captured,
            marker::boundary::Interval {
                start_ms: 3_000,
                end_ms: 7_000,
            },
        )
        .unwrap();
        assert!(candidate.start_ms >= 3_000 && candidate.end_ms <= 7_000);
        assert!(!marker::boundary::evidence::complete_guards(
            &captured,
            marker::boundary::Interval {
                start_ms: 5_000,
                end_ms: 10_000
            },
            marker::SegmentKind::Outro,
            Some(10_000),
            &marker::boundary::BoundaryPolicy::default(),
        ));
        let interior = capture(&file, 1_013, 10_027);
        let proof = interior.metrics.quantized_mapping.as_ref().unwrap();
        assert!(
            proof.origin_min_us <= 1_013_000 && proof.origin_max_us >= 1_012_000,
            "{proof:?}"
        );
        assert!(proof.origin_max_us - proof.origin_min_us <= 2001);
        assert_eq!(interior.metrics.sample_count, Some(144_224));
    }
}

#[test]
fn aac48_cross_block_gap_overlap_and_drift_remain_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let payloads = container::payloads_at_rate(dir.path(), 12, 48_000);
    for (name, direction) in [("gap", 1), ("overlap", -1), ("drift", 0)] {
        let file = dir.path().join(format!("{name}.mka"));
        container::write_at_rate(
            &file,
            &payloads,
            false,
            0,
            |group| {
                if direction == 0 {
                    group as i64 / 3
                } else if group >= 12 {
                    direction * 8
                } else {
                    0
                }
            },
            48_000,
        );
        let result = marker::ChromaprintEngine.capture_window(&CaptureRequest {
            path: file,
            window: SampleWindow::new(0, 10_000),
            audio_stream_index: None,
            process_deadline_ms: 30_000,
        });
        let error = match result {
            Err(error) => error,
            Ok(captured) => panic!(
                "changed anchors admitted: {:?}",
                captured.metrics.pcm_timeline
            ),
        };
        assert!(
            error.message.contains("source_block_clock_discontinuity"),
            "{name}: {error}"
        );
    }
}

#[test]
fn aac48_five_second_leading_audio_cannot_borrow_missing_guards() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("leading.mkv");
    ff(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:r=1:d=18",
            "-itsoffset",
            "5",
            "-f",
            "lavfi",
            "-i",
            "anoisesrc=seed=42:sample_rate=48000:duration=12",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-ac",
            "2",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
            "-threads",
            "1",
        ],
        &file,
    );
    let captured = capture(&file, 0, 10_000);
    let proof = captured.metrics.quantized_mapping.as_ref().unwrap();
    assert!(
        proof.origin_min_us >= 4_999_000 && proof.origin_max_us >= 5_000_000,
        "{proof:?}"
    );
    assert!(!marker::boundary::evidence::complete_pcm(&captured));
    let coverage = marker::boundary::evidence::candidate_pcm_coverage(&captured).unwrap();
    assert!(coverage.start_ms >= 4_999);
    assert!(!marker::boundary::evidence::complete_guards(
        &captured,
        marker::boundary::Interval {
            start_ms: 6_000,
            end_ms: 7_000
        },
        marker::SegmentKind::Intro,
        Some(18_000),
        &marker::boundary::BoundaryPolicy::default(),
    ));
}

#[test]
fn aac48_proof_must_match_source_rate_and_config() {
    let dir = tempfile::tempdir().unwrap();
    let payloads = container::payloads_at_rate(dir.path(), 12, 48_000);
    let file = dir.path().join("source.mka");
    container::write_at_rate(&file, &payloads, true, 0, |_| 0, 48_000);
    let captured = capture(&file, 0, 10_000);
    let relative = marker::boundary::Interval {
        start_ms: 3_000,
        end_ms: 7_000,
    };
    assert!(marker::boundary::normalize_capture_interval(&captured, relative).is_ok());
    let mut wrong_proof = captured.clone();
    wrong_proof
        .metrics
        .quantized_mapping
        .as_mut()
        .unwrap()
        .native_sample_rate = 44_100;
    assert!(marker::boundary::normalize_capture_interval(&wrong_proof, relative).is_err());
    let mut wrong_source = captured.metrics.source_facts.clone().unwrap();
    wrong_source.native_sample_rate = Some(44_100);
    let mut mismatched = captured.clone();
    mismatched.metrics.source_facts = Some(wrong_source.clone());
    assert!(marker::boundary::normalize_capture_interval(&mismatched, relative).is_err());
    let error = marker::fingerprint::capture::capture_from_source(
        &CaptureRequest {
            path: file.clone(),
            window: captured.window.clone(),
            audio_stream_index: Some(wrong_source.audio_stream_index),
            process_deadline_ms: 30_000,
        },
        &wrong_source,
        &marker::WorkControl::default(),
        std::ffi::OsStr::new("ffmpeg"),
    )
    .unwrap_err();
    assert!(error.message.contains("changed source facts"), "{error}");
    assert!(error.metrics.quantized_mapping.is_none());
    let source = captured.metrics.source_facts.as_ref().unwrap();
    assert_eq!(
        marker::capture_profile::profile_for_source(Some(source)).time_mapping_version,
        4
    );
    for (rate, channels) in [
        (Some(32_000), Some(2)),
        (None, Some(2)),
        (Some(48_000), Some(1)),
        (Some(48_000), None),
    ] {
        let mut unknown = source.clone();
        unknown.native_sample_rate = rate;
        unknown.audio_channels = channels;
        assert_eq!(
            marker::capture_profile::profile_for_source(Some(&unknown)).time_mapping_version,
            3
        );
    }
    // Same payload and declared 48k stereo, but an unproved 960-sample ASC.
    let mut bytes = std::fs::read(&file).unwrap();
    let at = bytes
        .windows(5)
        .position(|b| b == [0x63, 0xa2, 0x82, 0x11, 0x90])
        .unwrap();
    bytes[at + 4] = 0x94;
    std::fs::write(&file, bytes).unwrap();
    let error = marker::ChromaprintEngine
        .capture_window(&CaptureRequest {
            path: file,
            window: captured.window,
            audio_stream_index: None,
            process_deadline_ms: 30_000,
        })
        .unwrap_err();
    assert!(error.metrics.quantized_mapping.is_none());
}
