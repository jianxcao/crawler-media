use super::*;
use api::chapter_frame_service::{
    ChapterFrameRenderResult, ChapterFrameRequest, ChapterFrameRunner, ChapterFrameStaged,
    chapter_frame_artifact_path,
};
use std::{sync::Arc, time::Duration};

#[path = "capture_policy.rs"]
mod capture_policy;
#[path = "completion.rs"]
mod completion;

struct Engine;
impl marker::FingerprintEngine for Engine {
    fn extract_at(
        &self,
        _: &Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<marker::CommonSegment> {
        Some(marker::CommonSegment {
            start1_sec: 30.,
            end1_sec: 90.,
            start2_sec: 30.,
            end2_sec: 90.,
            duration_sec: 60.,
            score: 0.,
        })
    }
}
fn facts() -> marker::CaptureSourceFacts {
    marker::CaptureSourceFacts {
        audio_stream_index: 1,
        format_start_time_us: Some(0),
        stream_start_pts: Some(0),
        stream_time_base: Some("1/16000".into()),
        codec_name: None,
        native_sample_rate: None,
        audio_channels: None,
        provenance: "injected_local_header".into(),
    }
}
impl marker::FingerprintCaptureEngine for Engine {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        Ok(marker::CapturedFingerprint {
            window: request.window.clone(),
            words: vec![1, 2, 3, 4],
            pcm_duration_ms: Some(request.window.duration_ms()),
            metrics: marker::CaptureMetrics {
                time_mapping_version: 3,
                audio_stream_index: Some(1),
                source_facts: Some(facts()),
                pcm_timeline: marker::PcmTimeline::Contiguous,
                contiguous_pcm_start_ms: Some(request.window.start_ms),
                capture_process_ms: Some(0),
                remote_gate_wait_ms: Some(0),
                ..Default::default()
            },
        })
    }
}
impl api::probe_manager::TracksProbe for Engine {
    fn probe(
        &self,
        _: &Path,
        _: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack::default()),
                audio: vec![library::AudioTrack {
                    stream_index: Some(1),
                    capture_source: Some(facts()),
                    is_default: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            Some(400_000),
        ))
    }
}
struct Frames {
    fail: bool,
}
impl ChapterFrameRunner for Frames {
    fn render(
        &self,
        request: &ChapterFrameRequest,
        _: &library::WorkControl,
    ) -> Result<Vec<ChapterFrameRenderResult>, String> {
        std::thread::sleep(Duration::from_millis(120));
        if self.fail {
            return Err("injected_frame_failure".into());
        }
        request
            .candidates
            .iter()
            .map(|(index, artifact)| {
                let target = chapter_frame_artifact_path(
                    &request.video,
                    &request.generation,
                    *index,
                    artifact,
                );
                let staged = target.with_extension("staging.jpg");
                image::RgbImage::from_pixel(32, 18, image::Rgb([40, 80, 120]))
                    .save_with_format(&staged, image::ImageFormat::Jpeg)
                    .map_err(|e| e.to_string())?;
                Ok(ChapterFrameRenderResult {
                    index: *index,
                    staged: Ok(ChapterFrameStaged { staged, target }),
                })
            })
            .collect()
    }
}
fn session(opts: BenchmarkOptions, fail: bool) -> BenchmarkSession {
    let mut session = BenchmarkSession::prepare(opts).unwrap();
    session.state = session
        .state
        .with_probe_engines(Arc::new(Engine), Arc::new(Engine), Arc::new(Engine))
        .unwrap()
        .with_chapter_frame_runner(Arc::new(Frames { fail }));
    for row in session.registered_files() {
        session
            .state
            .store()
            .put_cached_chapters(&row.file.file_id.to_string(), &[])
            .unwrap();
    }
    session
}

#[test]
fn current_runner_waits_for_real_frame_terminal_and_retains_failed_elapsed() {
    for mode in ["full_window", "adaptive"] {
        for fail in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut opts = options(root.path(), &[1, 2, 3, 4]);
            opts.mode = mode.into();
            let run = session(opts, fail).run().unwrap();
            assert_eq!(run.media.len(), 1, "{mode}: {run:?}");
            let media = &run.media[0];
            assert_eq!(media.independent_episode_count, 4);
            assert_eq!(media.file_count, 4);
            assert!(media.elapsed_ms >= 120, "{mode}: {run:?}");
            assert!(media.frame_targets > 0, "{mode}: {run:?}");
            assert_eq!(run.wallclock_complete, !fail, "{mode}: {run:?}");
            assert_eq!(media.wallclock_complete, !fail);
            assert_eq!(
                media.terminal_status,
                if fail {
                    api::fingerprint_job::benchmark::TerminalStatus::Failed
                } else {
                    api::fingerprint_job::benchmark::TerminalStatus::Completed
                }
            );
            assert_eq!(media.ready_frames == media.frame_targets, !fail);
            assert_eq!(run.predictions.len(), 8);
            if mode == "full_window" {
                assert_eq!(
                    run.raw_pairs_computed,
                    Some(24),
                    "four episodes, two kinds, twelve ordered pairs each"
                );
            }
            if !fail {
                assert!(
                    run.predictions.iter().all(|p| p.state == "matched"),
                    "{mode}: {run:?}"
                );
            }
        }
    }
}

#[test]
fn incremental_arrival_keeps_existing_ids_and_same_episode_files_separate() {
    let root = tempfile::tempdir().unwrap();
    let opts = options(root.path(), &[1, 2, 3]);
    let first = session(opts, false);
    let before = first.registered_files();
    assert!(first.run().unwrap().wallclock_complete);
    let mut opts = options(root.path(), &[1, 2, 3, 4]);
    opts.scenario = "incremental".into();
    opts.run_id = "arrival".into();
    let next = session(opts.clone(), false);
    let after = next.registered_files();
    for old in before {
        assert!(
            after
                .iter()
                .any(|new| new.file.file_id == old.file.file_id && new.units == old.units)
        );
    }
    let run = next.run().unwrap();
    assert!(run.wallclock_complete, "{run:?}");
    assert_eq!(run.media[0].independent_episode_count, 4);
    assert_eq!(
        run.jobs.len(),
        5,
        "metadata and fingerprint for new source, three prior consensus successors"
    );
    assert!(run.jobs.iter().all(|job| job["status"] == "succeeded"));
    for row in &after {
        let jobs = run
            .jobs
            .iter()
            .flat_map(|job| job["units"].as_array().unwrap())
            .filter(|unit| unit["ledger_id"] == row.file.file_id.to_string())
            .count();
        assert_eq!(jobs, if row.units[0].episode == 4 { 2 } else { 1 });
    }
    let mut rows: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&opts.sources).unwrap()).unwrap();
    for row in rows
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .filter(|row| row["unit"]["episode"] == 4)
    {
        row["unit"]["episode"] = json!(3);
    }
    // A distinct isolated instance permits a second cut of episode 3 without remapping an existing file.
    opts.data_dir = root.path().join("two-cuts");
    opts.scenario = "cold".into();
    std::fs::write(&opts.sources, serde_json::to_vec(&rows).unwrap()).unwrap();
    let separate = session(opts, false);
    let files = separate.registered_files();
    let cuts = files
        .iter()
        .filter(|row| row.units[0].episode == 3)
        .collect::<Vec<_>>();
    assert_eq!(cuts.len(), 2);
    assert_ne!(cuts[0].file.file_id, cuts[1].file.file_id);
    let run = separate.run().unwrap();
    assert_eq!(run.media[0].independent_episode_count, 3);
    assert_eq!(run.media[0].file_count, 4);
}

struct RefuseMedia;
impl marker::FingerprintCaptureEngine for RefuseMedia {
    fn capture_window(
        &self,
        _: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        Err(marker::CaptureFailure {
            kind: marker::CaptureFailureKind::Io,
            message: "warm run must reuse valid audio".into(),
            metrics: Default::default(),
        })
    }
}
impl api::probe_manager::TracksProbe for RefuseMedia {
    fn probe(
        &self,
        _: &Path,
        _: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        Err(library::LibraryError::Probe(
            "warm run must reuse source-versioned metadata".into(),
        ))
    }
}

#[test]
fn warm_refresh_finishes_without_media_reads_and_reports_measured_zero_capture() {
    let root = tempfile::tempdir().unwrap();
    let mut opts = options(root.path(), &[1, 2, 3]);
    assert!(
        session(opts.clone(), false)
            .run()
            .unwrap()
            .wallclock_complete
    );
    opts.scenario = "warm".into();
    opts.run_id = "warm".into();
    let mut warm = session(opts, false);
    warm.state = warm
        .state
        .with_probe_engines(
            Arc::new(Engine),
            Arc::new(RefuseMedia),
            Arc::new(RefuseMedia),
        )
        .unwrap();
    let run = warm.run().unwrap();
    assert!(run.wallclock_complete, "{run:?}");
    assert_eq!(run.stage_totals.capture, Some(0));
    assert_eq!(run.stage_totals.remote_gate_wait, Some(0));
    assert!(run.raw_pairs_computed.is_some_and(|count| count > 0));
    assert_eq!(run.input_bytes, None);
    assert!(!run.measurement_complete);
}

#[path = "kind_outcomes.rs"]
mod kind_outcomes;
