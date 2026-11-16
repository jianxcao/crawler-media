use super::clock::{AnalysisClock, MeasurementClock};
use super::*;
use api::fingerprint_job::benchmark::TerminalStatus;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};

#[test]
fn media_waits_for_its_successor_analysis_while_other_media_completes() {
    let root = tempfile::tempdir().unwrap();
    let mut opts = two_group_options(root.path());
    opts.deadline_ms = 1500;
    let measurement_clock = MeasurementClock::new(opts.deadline_ms);
    let mut evaluation = session(opts, false).with_clock(measurement_clock.clone());
    let rows = evaluation.registered_files();
    let first = rows
        .iter()
        .find(|row| row.units[0].episode == 1)
        .unwrap()
        .clone();
    let open = [
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ];
    let (entered, receiver) = mpsc::channel();
    evaluation.state = evaluation
        .state
        .with_chapter_frame_runner(Arc::new(GatedFrames {
            first_group: rows
                .iter()
                .filter(|row| row.units[0].episode <= 3)
                .map(|row| row.file.file_id)
                .collect(),
            open: open.clone(),
            second_gated: AtomicBool::new(false),
            first_gated: AtomicBool::new(false),
            entered,
        }));
    let clock = AnalysisClock::advance(&evaluation);
    let state = evaluation.state.clone();
    let running = std::thread::spawn(move || evaluation.run().unwrap());
    let mut seen = [false; 2];
    while !seen.iter().all(|seen| *seen) {
        let group = receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_else(|error| {
                panic!("frame entry: {error:?}; {}", stage_snapshot(&state, &rows))
            });
        seen[group] = true;
    }
    drop(clock);
    let successor = state
        .probe
        .enqueue_direct_fingerprint(
            api::probe_manager::ProbeUnit {
                row: first.clone(),
                kind: domain::MediaKind::Tv,
                force_fingerprint: true,
                reuse_fingerprint_cache: true,
                overwrite_markers: false,
                reuse_media_info_cache: true,
                marker_refresh_id: None,
                job_id: None,
            },
            Some(400_000),
            first.file.version.source_version.clone(),
            180,
        )
        .unwrap();
    let started = std::time::Instant::now();
    loop {
        if state
            .store()
            .active_probe_unit_for_ledger(&first.file.file_id.to_string())
            .unwrap()
            .is_some_and(|unit| {
                unit.job_id == successor && unit.stage == store::ProbeDispatchStage::Analysis
            })
        {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(5));
    }
    for gate in &open {
        gate.store(true, Ordering::SeqCst);
    }
    wait_until_for(Duration::from_secs(2), || {
        frames_complete(&state, &rows)
            && state.chapter_frames().pending_memory() == 0
            && rows
                .iter()
                .filter(|row| row.file.media_id != first.file.media_id)
                .all(|row| probes_complete(&state, row))
    });
    measurement_clock.resume();
    let run = running.join().unwrap();
    let pending = run
        .media
        .iter()
        .find(|media| media.media_group == "m1")
        .unwrap();
    let completed = run
        .media
        .iter()
        .find(|media| media.media_group == "m2")
        .unwrap();
    assert_eq!(pending.terminal_status, TerminalStatus::Incomplete);
    assert_eq!(pending.elapsed_ms, 1500);
    assert_eq!(run.total_elapsed_ms, 1500);
    assert!(pending.job_ids.contains(&successor));
    assert_eq!(completed.terminal_status, TerminalStatus::Completed);
    assert!(!completed.job_ids.contains(&successor));
    assert!(
        run.jobs
            .iter()
            .any(|job| job["id"] == successor && job["units"][0]["status"] == "queued")
    );
}

struct GatedFrames {
    first_group: Vec<domain::LedgerId>,
    open: [Arc<AtomicBool>; 2],
    second_gated: AtomicBool,
    first_gated: AtomicBool,
    entered: mpsc::Sender<usize>,
}

impl ChapterFrameRunner for GatedFrames {
    fn render(
        &self,
        request: &ChapterFrameRequest,
        control: &library::WorkControl,
    ) -> Result<Vec<ChapterFrameRenderResult>, String> {
        let group = usize::from(!self.first_group.contains(&request.file_id));
        // Keep one worker available if the second Media is dispatched first.
        let wait = if group == 0 {
            !self.first_gated.swap(true, Ordering::SeqCst)
        } else {
            !self.second_gated.swap(true, Ordering::SeqCst)
        };
        let _ = self.entered.send(group);
        while wait && !self.open[group].load(Ordering::SeqCst) {
            if control.is_cancelled() {
                return Err("gated frame cancelled".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Frames { fail: false }.render(request, control)
    }
}

fn two_group_options(root: &Path) -> BenchmarkOptions {
    let opts = options(root, &[1, 2, 3, 4, 5, 6]);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&opts.manifest).unwrap()).unwrap();
    for case in manifest["cases"].as_array_mut().unwrap().iter_mut().skip(6) {
        case["media_group"] = json!("m2");
    }
    std::fs::write(&opts.manifest, serde_json::to_vec(&manifest).unwrap()).unwrap();
    opts
}

#[test]
fn a_media_finishes_while_another_medias_frames_remain_gated() {
    let root = tempfile::tempdir().unwrap();
    let mut opts = two_group_options(root.path());
    opts.deadline_ms = 1500;
    let clock = MeasurementClock::new(opts.deadline_ms);
    let mut evaluation = session(opts, false).with_clock(clock.clone());
    let rows = evaluation.registered_files();
    let first_group: Vec<_> = rows
        .iter()
        .filter(|row| row.units[0].episode <= 3)
        .cloned()
        .collect();
    let open = [
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ];
    let (entered, receiver) = mpsc::channel();
    evaluation.state = evaluation
        .state
        .with_chapter_frame_runner(Arc::new(GatedFrames {
            first_group: first_group.iter().map(|row| row.file.file_id).collect(),
            open: open.clone(),
            second_gated: AtomicBool::new(false),
            first_gated: AtomicBool::new(false),
            entered,
        }));
    let _analysis_clock = AnalysisClock::advance(&evaluation);
    let state = evaluation.state.clone();
    let running = std::thread::spawn(move || evaluation.run().unwrap());
    let mut seen = [false; 2];
    while !seen[0] {
        seen[receiver.recv_timeout(Duration::from_secs(2)).unwrap()] = true;
    }
    open[0].store(true, Ordering::SeqCst);
    while !seen[1] {
        seen[receiver.recv_timeout(Duration::from_secs(2)).unwrap()] = true;
    }
    wait_until_for(Duration::from_secs(2), || {
        frames_complete(&state, &first_group)
            && state.chapter_frames().pending_memory() == 1
            && first_group.iter().all(|row| probes_complete(&state, row))
    });
    clock.resume();
    let run = running.join().unwrap();
    assert!(!open[1].load(Ordering::SeqCst));
    let first = run
        .media
        .iter()
        .find(|media| media.media_group == "m1")
        .unwrap();
    let second = run
        .media
        .iter()
        .find(|media| media.media_group == "m2")
        .unwrap();
    assert_eq!(first.terminal_status, TerminalStatus::Completed, "{run:?}");
    assert!(first.wallclock_complete);
    assert!(first.frame_targets > 0);
    assert_eq!(first.ready_frames, first.frame_targets);
    assert!(first.elapsed_ms < second.elapsed_ms);
    assert!(!second.wallclock_complete);
    assert_eq!(second.elapsed_ms, 1500);
    assert_eq!(run.total_elapsed_ms, 1500);
    assert_eq!(run.terminal_status, TerminalStatus::Incomplete);
}

struct PublishOneFile {
    first_started: AtomicBool,
}

impl ChapterFrameRunner for PublishOneFile {
    fn render(
        &self,
        request: &ChapterFrameRequest,
        control: &library::WorkControl,
    ) -> Result<Vec<ChapterFrameRenderResult>, String> {
        if !self.first_started.swap(true, Ordering::SeqCst) {
            return Frames { fail: false }.render(request, control);
        }
        while !control.is_cancelled() {
            std::thread::sleep(Duration::from_millis(5));
        }
        Err("deadline left this frame incomplete".into())
    }
}

#[test]
fn deadline_retains_persisted_frame_targets_and_published_jpegs() {
    let root = tempfile::tempdir().unwrap();
    let mut opts = options(root.path(), &[1, 2, 3]);
    opts.deadline_ms = 1000;
    let clock = MeasurementClock::new(opts.deadline_ms);
    let mut evaluation = session(opts, false);
    evaluation.state = evaluation
        .state
        .with_chapter_frame_runner(Arc::new(PublishOneFile {
            first_started: AtomicBool::new(false),
        }));
    evaluation = evaluation.with_clock(clock.clone());
    let _analysis_clock = AnalysisClock::advance(&evaluation);
    let state = evaluation.state.clone();
    let files = evaluation.registered_files();
    let running = std::thread::spawn(move || evaluation.run().unwrap());
    wait_until(|| {
        let tasks = files
            .iter()
            .map(|file| {
                state
                    .store()
                    .current_chapter_frame_task(file.file.file_id)
                    .unwrap()
            })
            .collect::<Option<Vec<_>>>();
        tasks.is_some_and(|tasks| {
            tasks
                .iter()
                .any(|task| task.targets.iter().any(|target| target.ready))
                && tasks
                    .iter()
                    .any(|task| task.status.state == store::ChapterFrameState::Running)
        })
    });
    clock.expire();
    let run = running.join().unwrap();
    let targets: usize = run
        .frames
        .iter()
        .map(|frame| frame["targets"].as_array().unwrap().len())
        .sum();
    let ready = run
        .frames
        .iter()
        .flat_map(|frame| frame["targets"].as_array().unwrap())
        .filter(|target| target["ready"] == true)
        .count();
    assert!(targets > ready && ready > 0, "{:?}", run.frames);
    for frame in &run.frames {
        let file = files
            .iter()
            .find(|file| frame["file_id"] == json!(file.file.file_id))
            .unwrap();
        let task = state
            .store()
            .current_chapter_frame_task(file.file.file_id)
            .unwrap()
            .unwrap();
        for target in task.targets.iter().filter(|target| target.ready) {
            let path = chapter_frame_artifact_path(
                &file.file.path,
                &task.status.generation,
                target.index,
                target.published.as_ref().unwrap(),
            );
            assert!(image::open(path).is_ok());
        }
    }
    assert!(
        run.frames
            .iter()
            .any(|frame| frame["status"]["state"] == "running")
    );
    let media = &run.media[0];
    assert_eq!(media.frame_targets, targets);
    assert_eq!(media.ready_frames, ready);
    assert_eq!(media.elapsed_ms, 1000);
    assert_eq!(run.total_elapsed_ms, 1000);
    assert!(run.total_elapsed_ms >= media.elapsed_ms);
    assert_eq!(media.terminal_status, TerminalStatus::Incomplete);
    assert_eq!(run.terminal_status, TerminalStatus::Incomplete);
    assert!(!media.wallclock_complete && !run.wallclock_complete);
    assert!(
        run.predictions
            .iter()
            .all(|prediction| prediction.state == "error")
    );
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    wait_until_for(Duration::from_secs(5), &mut condition);
}

fn wait_until_for(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let waiting = std::time::Instant::now();
    while !condition() {
        assert!(waiting.elapsed() < timeout);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn frames_complete(state: &api::ApiState, rows: &[domain::FileCoverage]) -> bool {
    rows.iter().all(|row| {
        state
            .store()
            .current_chapter_frame_task(row.file.file_id)
            .unwrap()
            .is_some_and(|task| {
                task.status.state == store::ChapterFrameState::Completed
                    && !task.targets.is_empty()
                    && task.targets.iter().all(|target| target.ready)
            })
    })
}

fn probes_complete(state: &api::ApiState, row: &domain::FileCoverage) -> bool {
    let store = state.store();
    store
        .active_probe_unit_for_ledger(&row.file.file_id.to_string())
        .unwrap()
        .is_none()
        && store
            .latest_marker_refresh_for_season(
                &row.file.media_id.to_string(),
                row.units[0].season as u32,
            )
            .unwrap()
            .is_some_and(|job| !job.is_active())
}

fn stage_snapshot(state: &api::ApiState, rows: &[domain::FileCoverage]) -> serde_json::Value {
    json!({
        "frame_workers": state.chapter_frames().pending_memory(),
        "files": rows.iter().map(|row| json!({
            "episode": row.units[0].episode,
            "unit": format!("{:?}", state.store().active_probe_unit_for_ledger(&row.file.file_id.to_string()).unwrap()),
            "frame": format!("{:?}", state.store().current_chapter_frame_task(row.file.file_id).unwrap()),
            "refresh": format!("{:?}", state.store().latest_marker_refresh_for_season(
                &row.file.media_id.to_string(), row.units[0].season as u32
            ).unwrap())
        })).collect::<Vec<_>>()
    })
}

struct GatedCapture {
    entered: mpsc::Sender<()>,
    release: Arc<AtomicBool>,
}

impl marker::FingerprintCaptureEngine for GatedCapture {
    fn capture_window(
        &self,
        _: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        unreachable!("capture must use cancellation control");
    }

    fn capture_window_controlled(
        &self,
        _: &marker::CaptureRequest,
        control: &marker::WorkControl,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        let _ = self.entered.send(());
        while !self.release.load(Ordering::SeqCst) && !control.is_cancelled() {
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(marker::CaptureFailure {
            kind: marker::CaptureFailureKind::Io,
            message: "capture cancelled before frame publication".into(),
            metrics: Default::default(),
        })
    }
}

#[test]
fn deadline_before_frame_persistence_reports_incomplete_without_targets() {
    let root = tempfile::tempdir().unwrap();
    let mut opts = options(root.path(), &[1, 2, 3]);
    opts.deadline_ms = 1000;
    let output = opts.output.join("run1-predictions.json");
    let clock = MeasurementClock::new(opts.deadline_ms);
    let mut evaluation = session(opts, false).with_clock(clock.clone());
    let (entered, receiver) = mpsc::channel();
    let release = Arc::new(AtomicBool::new(false));
    evaluation.state = evaluation
        .state
        .with_probe_engines(
            Arc::new(Engine),
            Arc::new(GatedCapture {
                entered,
                release: release.clone(),
            }),
            Arc::new(Engine),
        )
        .unwrap();
    let running = std::thread::spawn(move || evaluation.run().unwrap());
    receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    clock.expire();
    // The public output is written before graceful worker teardown.
    wait_until(|| output.exists());
    release.store(true, Ordering::SeqCst);
    let run = running.join().unwrap();
    assert_eq!(run.terminal_status, TerminalStatus::Incomplete);
    assert!(!run.wallclock_complete);
    assert_eq!(run.total_elapsed_ms, 1000);
    assert_eq!(run.media[0].terminal_status, TerminalStatus::Incomplete);
    assert_eq!(run.media[0].elapsed_ms, 1000);
    assert_eq!(run.media[0].frame_targets, 0);
    assert_eq!(run.media[0].ready_frames, 0);
    assert_eq!(run.frames.len(), 3);
    assert!(
        run.frames
            .iter()
            .all(|frame| frame["status"].is_null() && frame["targets"].is_null())
    );
    assert!(
        run.predictions
            .iter()
            .all(|prediction| prediction.state == "error")
    );
}
