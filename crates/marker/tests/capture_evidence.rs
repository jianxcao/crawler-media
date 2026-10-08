use std::path::PathBuf;

use marker::{
    CaptureFailure, CaptureFailureKind, CaptureMetrics, CaptureRequest, CapturedFingerprint,
    FingerprintCaptureEngine, SampleWindow,
};

struct PartialCoverageEngine {
    actual_duration_ms: i64,
    words: Vec<u32>,
}

impl FingerprintCaptureEngine for PartialCoverageEngine {
    fn capture_window(
        &self,
        request: &CaptureRequest,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        request.window.validate()?;
        Ok(CapturedFingerprint {
            window: request.window.clone(),
            words: self.words.clone(),
            pcm_duration_ms: Some(self.actual_duration_ms),
            metrics: CaptureMetrics {
                elapsed_ms: 120,
                time_to_first_pcm_ms: Some(40),
                pcm_read_wait_us: 1500,
                chromaprint_consume_us: 3200,
                pcm_bytes: (self.actual_duration_ms as u64 * 16 * 2), // 16 samples/ms * 2 bytes
                input_bytes: None,
                input_bytes_source: None,
                measurement_complete: false,
                ffmpeg_exit_code: Some(0),
                ..Default::default()
            },
        })
    }
}

#[test]
fn captured_window_reports_actual_pcm_coverage() {
    let engine = PartialCoverageEngine {
        actual_duration_ms: 12_000,
        words: vec![123, 456, 789],
    };

    let request = CaptureRequest {
        path: PathBuf::from("fake_episode.mkv"),
        window: SampleWindow {
            start_ms: 100_000,
            end_ms: 140_000,
        },
        audio_stream_index: None,
        process_deadline_ms: 30_000,
    };

    let capture = engine
        .capture_window(&request)
        .expect("capture should succeed");

    assert_eq!(capture.window.start_ms, 100_000);
    assert_eq!(capture.window.end_ms, 140_000);
    assert_eq!(capture.pcm_duration_ms, Some(12_000));
    assert!(capture.metrics.pcm_bytes > 0);
    assert_eq!(capture.metrics.input_bytes, None);
}

#[test]
fn sample_window_validation_rejects_negative_and_empty_ranges() {
    let negative = SampleWindow {
        start_ms: -1000,
        end_ms: 10_000,
    };
    assert!(matches!(
        negative.validate(),
        Err(CaptureFailure {
            kind: CaptureFailureKind::InvalidWindow,
            ..
        })
    ));

    let empty = SampleWindow {
        start_ms: 50_000,
        end_ms: 50_000,
    };
    assert!(matches!(
        empty.validate(),
        Err(CaptureFailure {
            kind: CaptureFailureKind::InvalidWindow,
            ..
        })
    ));

    let inverted = SampleWindow {
        start_ms: 60_000,
        end_ms: 50_000,
    };
    assert!(matches!(
        inverted.validate(),
        Err(CaptureFailure {
            kind: CaptureFailureKind::InvalidWindow,
            ..
        })
    ));

    let valid = SampleWindow {
        start_ms: 0,
        end_ms: 180_000,
    };
    assert!(valid.validate().is_ok());
}

#[test]
fn unknown_input_bytes_remain_null() {
    let engine = PartialCoverageEngine {
        actual_duration_ms: 5_000,
        words: vec![1, 2],
    };
    let request = CaptureRequest {
        path: PathBuf::from("episode.strm"),
        window: SampleWindow {
            start_ms: 0,
            end_ms: 10_000,
        },
        audio_stream_index: Some(0),
        process_deadline_ms: 10_000,
    };
    let capture = engine.capture_window(&request).unwrap();
    assert!(capture.metrics.pcm_bytes > 0);
    assert_eq!(capture.metrics.input_bytes, None);
    assert_eq!(capture.metrics.input_bytes_source, None);
    assert!(!capture.metrics.measurement_complete);
}

#[test]
fn capture_types_support_serde_roundtrip() {
    let metrics = CaptureMetrics {
        elapsed_ms: 1500,
        time_to_first_pcm_ms: Some(250),
        pcm_read_wait_us: 10000,
        chromaprint_consume_us: 20000,
        pcm_bytes: 32000,
        input_bytes: Some(1048576),
        input_bytes_source: Some(marker::InputBytesSource::AvioInput),
        measurement_complete: true,
        ffmpeg_exit_code: Some(0),
        ..Default::default()
    };
    let captured = CapturedFingerprint {
        window: SampleWindow {
            start_ms: 1000,
            end_ms: 6000,
        },
        words: vec![100, 200, 300],
        pcm_duration_ms: Some(5000),
        metrics,
    };

    let json = serde_json::to_string(&captured).expect("serialization should work");
    let deserialized: CapturedFingerprint =
        serde_json::from_str(&json).expect("deserialization should work");

    assert_eq!(deserialized.window.start_ms, 1000);
    assert_eq!(deserialized.window.end_ms, 6000);
    assert_eq!(deserialized.pcm_duration_ms, Some(5000));
    assert_eq!(deserialized.words, vec![100, 200, 300]);
    assert_eq!(deserialized.metrics.input_bytes, Some(1048576));
    assert_eq!(
        deserialized.metrics.input_bytes_source,
        Some(marker::InputBytesSource::AvioInput)
    );
}
