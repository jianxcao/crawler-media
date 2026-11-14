use super::*;
use api::fingerprint_job::benchmark::TerminalStatus;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};

struct AnalysisClock {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl AnalysisClock {
    fn advance(session: &BenchmarkSession) -> Self {
        let clock = Arc::new(api::probe_manager::clock::FakeClock::new(0));
        let manager = session.state.probe.clone();
        manager.set_clock(clock.clone());
        let store = session.state.store().clone();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut now = 0;
            while !done.load(Ordering::SeqCst) {
                if !store
                    .pending_probe_analysis_keys(None, 1)
                    .unwrap()
                    .is_empty()
                {
                    now += 2_000;
                    clock.set(now);
                    manager.wake_dispatch();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for AnalysisClock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[test]
fn media_waits_for_its_successor_analysis_while_other_media_completes() {
    let root = tempfile::tempdir().unwrap();
    let mut opts = two_group_options(root.path());
    opts.deadline_ms = 1500;
    let mut evaluation = session(opts, false);
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
        seen[receiver.recv_timeout(Duration::from_secs(2)).unwrap()] = true;
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
    let mut evaluation = session(opts, false);
    let first_group = evaluation
        .registered_files()
        .iter()
        .filter(|row| row.units[0].episode <= 3)
        .map(|row| row.file.file_id)
        .collect();
    let open = [
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ];
    let (entered, receiver) = mpsc::channel();
    evaluation.state = evaluation
        .state
        .with_chapter_frame_runner(Arc::new(GatedFrames {
            first_group,
            open: open.clone(),
            second_gated: AtomicBool::new(false),
            first_gated: AtomicBool::new(false),
            entered,
        }));
    let _analysis_clock = AnalysisClock::advance(&evaluation);
    let running = std::thread::spawn(move || evaluation.run().unwrap());
    let mut seen = [false; 2];
    while !seen[0] {
        seen[receiver.recv_timeout(Duration::from_secs(2)).unwrap()] = true;
    }
    open[0].store(true, Ordering::SeqCst);
    while !seen[1] {
        seen[receiver.recv_timeout(Duration::from_secs(2)).unwrap()] = true;
    }
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
    let mut evaluation = session(opts, false);
    evaluation.state = evaluation
        .state
        .with_chapter_frame_runner(Arc::new(PublishOneFile {
            first_started: AtomicBool::new(false),
        }));
    let _analysis_clock = AnalysisClock::advance(&evaluation);
    let run = evaluation.run().unwrap();
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
    assert!(
        run.frames
            .iter()
            .any(|frame| frame["status"]["state"] == "running")
    );
    let media = &run.media[0];
    assert_eq!(media.frame_targets, targets);
    assert_eq!(media.ready_frames, ready);
    assert!(media.elapsed_ms >= 1000);
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
