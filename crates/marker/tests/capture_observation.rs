use marker::{
    CaptureRequest, CaptureSourceFacts, FingerprintCaptureEngine, PcmTimeline, SampleWindow,
};
use std::os::unix::fs::PermissionsExt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

#[test]
fn paused_capture_does_not_begin_an_attempt_before_admission() {
    let request = CaptureRequest {
        path: "unopened.STRM".into(),
        window: SampleWindow::new(0, 10_000),
        audio_stream_index: Some(0),
        process_deadline_ms: 30_000,
    };
    let facts = CaptureSourceFacts {
        audio_stream_index: 0,
        format_start_time_us: None,
        stream_start_pts: None,
        stream_time_base: None,
        codec_name: None,
        native_sample_rate: None,
        audio_channels: None,
        provenance: "fixture".into(),
    };
    marker::media_read::set_remote_reads_paused(true);
    let stop = Arc::new(AtomicBool::new(false));
    let control = marker::WorkControl::default().with_read_stop(stop.clone());
    let (entered, entry) = std::sync::mpsc::channel();
    let (attempt, began) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        entered.send(()).unwrap();
        marker::ChromaprintEngine.capture_window_after_metadata_controlled(
            &request,
            Some(&facts),
            &control,
            &mut || {
                attempt.send(()).unwrap();
                Err(marker::CaptureFailure {
                    kind: marker::CaptureFailureKind::Io,
                    message: "unexpected capture admission".into(),
                    metrics: Default::default(),
                })
            },
        )
    });
    entry.recv_timeout(Duration::from_secs(1)).unwrap();
    let early_attempt = began.recv_timeout(Duration::from_millis(100));
    stop.store(true, Ordering::Release);
    let error = worker.join().unwrap().unwrap_err();
    marker::media_read::set_remote_reads_paused(false);
    assert!(matches!(
        early_attempt,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    assert_eq!(error.message, marker::media_read::READ_WAIT_STOPPED);
}

fn observed(lines: &str) -> marker::CapturedFingerprint {
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("decoder");
    let script = format!(
        "#!/usr/bin/env python3\nimport sys\nsys.stderr.write({lines:?})\nsys.stdout.buffer.write(bytes(range(256))*1250)\n"
    );
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let facts = CaptureSourceFacts {
        audio_stream_index: 0,
        format_start_time_us: Some(0),
        stream_start_pts: Some(0),
        stream_time_base: Some("1/16000".into()),
        codec_name: None,
        native_sample_rate: None,
        audio_channels: None,
        provenance: "test_decoder_header".into(),
    };
    marker::fingerprint::capture::capture_from_source(
        &CaptureRequest {
            path: root.path().join("source"),
            window: SampleWindow::new(0, 10_000),
            audio_stream_index: Some(0),
            process_deadline_ms: 30_000,
        },
        &facts,
        &marker::WorkControl::default(),
        program.as_os_str(),
    )
    .unwrap()
}
fn frame(n: u32, pts: i64, samples: u32) -> String {
    format!(
        "[ashowinfo@cm_timeline @ 0x123] n:{n} pts:{pts} pts_time:0 fmt:s16 channels:1 chlayout:mono rate:16000 nb_samples:{samples} checksum:123\n"
    )
}

#[test]
fn unusable_same_process_evidence_never_proves_boundary_coverage() {
    for lines in [
        String::new(),
        frame(0, 0, 159_999),
        frame(0, 0, 160_000).replace("pts:0", "pts:NOPTS"),
        format!("{}{}", "x".repeat(5000), frame(0, 0, 160_000)),
        frame(0, 0, 160_000).replace("[ashowinfo@cm_timeline", "[ashowinfo@unrelated"),
    ] {
        let capture = observed(&lines);
        assert_eq!(capture.metrics.pcm_timeline, PcmTimeline::Unknown);
        assert!(capture.metrics.contiguous_pcm_start_ms.is_none());
        assert!(!marker::boundary::evidence::complete_pcm(&capture));
        assert!(marker::boundary::evidence::candidate_pcm_coverage(&capture).is_none());
    }
}

#[test]
fn observed_invalidity_survives_later_unusable_diagnostics() {
    let capture = observed(&format!(
        "{}{}{}",
        frame(0, 0, 80_000),
        frame(1, 80_256, 80_000),
        frame(2, 160_256, 1).replace("pts:160256", "pts:NOPTS")
    ));
    assert!(matches!(
        capture.metrics.pcm_timeline,
        PcmTimeline::Invalid(_)
    ));
    assert!(!marker::boundary::evidence::complete_pcm(&capture));
    let valid = observed(&frame(0, 0, 160_000));
    assert_eq!(valid.metrics.contiguous_pcm_start_ms, Some(0));
    assert!(marker::boundary::evidence::complete_pcm(&valid));
}
