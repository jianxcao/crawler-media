use super::*;
use api::probe_manager::ProbeManager;
use marker::FingerprintCaptureEngine;
use std::{
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};

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
struct MeasuredCapture {
    capture: marker::CapturedFingerprint,
    store: Arc<store::Store>,
    media: domain::MediaId,
    requests: Mutex<Vec<marker::SampleWindow>>,
}
impl marker::FingerprintCaptureEngine for MeasuredCapture {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        self.requests.lock().unwrap().push(request.window.clone());
        if request.window == self.capture.window {
            return Ok(self.capture.clone());
        }
        let job = self
            .store
            .latest_marker_refresh_for_season(&self.media.to_string(), 1)
            .unwrap()
            .unwrap();
        self.store
            .cancel_probe_job(&job.id, "expanded request observed")
            .unwrap();
        Err(marker::CaptureFailure {
            kind: marker::CaptureFailureKind::Io,
            message: "expanded read observed; fixture stops here".into(),
            metrics: Default::default(),
        })
    }
}
struct MeasuredTracks(marker::CaptureSourceFacts);
impl api::probe_manager::TracksProbe for MeasuredTracks {
    fn probe(
        &self,
        _: &Path,
        _: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        Ok((tracks(&self.0), None))
    }
}
fn tracks(source: &marker::CaptureSourceFacts) -> library::Tracks {
    library::Tracks {
        video: Some(library::VideoTrack {
            codec: Some("h264".into()),
            width: Some(1920),
            height: Some(1080),
            ..Default::default()
        }),
        audio: vec![library::AudioTrack {
            stream_index: Some(source.audio_stream_index),
            is_default: true,
            capture_source: Some(source.clone()),
            ..Default::default()
        }],
        ..Default::default()
    }
}
fn measured() -> marker::CapturedFingerprint {
    let dir = tempfile::tempdir().unwrap();
    let encoded = dir.path().join("source.mp4");
    ff(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:r=1:d=182",
            "-f",
            "lavfi",
            "-i",
            "anoisesrc=seed=42:sample_rate=44100:duration=182",
            "-ac",
            "2",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
            "-threads",
            "1",
        ],
        &encoded,
    );
    let path = dir.path().join("measured.mkv");
    ff(
        &[
            "-ss",
            &format!("{:.6}", 1024. / 44_100.),
            "-i",
            encoded.to_str().unwrap(),
            "-c",
            "copy",
        ],
        &path,
    );
    let capture = marker::ChromaprintEngine
        .capture_window(&marker::CaptureRequest {
            path,
            window: marker::SampleWindow::new(0, 180_000),
            audio_stream_index: None,
            process_deadline_ms: 120_000,
        })
        .unwrap();
    assert_eq!(capture.metrics.time_mapping_version, 4);
    assert_eq!(capture.metrics.sample_count, Some(2_880_000));
    assert!(!marker::boundary::evidence::complete_pcm(&capture));
    capture
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn measured_mapping4_precision_allows_discovery_but_honest_leading_loss_does_not() {
    let measured = tokio::task::spawn_blocking(measured).await.unwrap();
    for leading in [false, true] {
        let (mut s, matcher) = scenario(3, -1, 0, Some(0)).await;
        s.manager.shutdown_workers().await.unwrap();
        let mut capture = measured.clone();
        if leading {
            // A controlled leading-loss proof counterexample, not a claim that
            // the producer emitted this arbitrary shifted interval.
            let origin = capture.metrics.quantized_mapping.as_mut().unwrap();
            origin.origin_min_us += 10_000;
            origin.origin_max_us += 10_000;
        }
        let source = capture.metrics.source_facts.clone().unwrap();
        for row in &s.ledgers {
            s.store
                .put_file_meta_versioned(
                    &row.file.file_id.to_string(),
                    &tracks(&source),
                    Some(&row.file.version.source_version),
                    None,
                )
                .unwrap();
        }
        let engine = Arc::new(MeasuredCapture {
            capture,
            store: s.store.clone(),
            media: s.media_id,
            requests: Mutex::new(vec![]),
        });
        s.manager = Arc::new(
            ProbeManager::with_engines(s.store.clone(), matcher, engine.clone())
                .unwrap()
                .with_tracks_probe(Arc::new(MeasuredTracks(source))),
        );
        s.manager.set_clock(s.clock.clone());
        assert!(s.manager.try_start_workers());
        let job = run(&s).await;
        let expanded = engine
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|w| w.start_ms == 120_000 && w.end_ms == 420_000);
        assert_eq!(
            expanded, !leading,
            "honest leading loss cannot authorize range discovery"
        );
        if leading {
            assert_eq!(job.status, "succeeded");
        }
        for row in &s.ledgers {
            assert!(
                s.store
                    .get_file_marker(row.file.file_id, row.units[0])
                    .unwrap()
                    .is_none()
            );
            assert!(
                s.store
                    .current_chapter_frame_task(row.file.file_id)
                    .unwrap()
                    .is_none()
            );
        }
    }
}
