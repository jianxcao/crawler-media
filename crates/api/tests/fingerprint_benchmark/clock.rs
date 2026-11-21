use super::*;
use api::fingerprint_job::benchmark::{BenchmarkClock, BenchmarkRun};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub(super) fn run(session: BenchmarkSession) -> BenchmarkRun {
    let _analysis_clock = AnalysisClock::advance(&session);
    session.run().unwrap()
}

/// Hold the first analysis quiet-window tick until every registered file has
/// reached `ProbeDispatchStage::Analysis`, making single-batch analysis deterministic.
pub(super) fn run_when_all_ready(session: BenchmarkSession) -> BenchmarkRun {
    let _analysis_clock = AnalysisClock::advance_when_all_ready(&session);
    session.run().unwrap()
}

pub(super) struct MeasurementClock {
    origin: Instant,
    deadline: Duration,
    resumed: std::sync::Mutex<Option<Instant>>,
    expired: AtomicBool,
}

impl MeasurementClock {
    pub(super) fn new(deadline_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            origin: Instant::now(),
            deadline: Duration::from_millis(deadline_ms),
            resumed: std::sync::Mutex::new(None),
            expired: AtomicBool::new(false),
        })
    }

    pub(super) fn expire(&self) {
        self.expired.store(true, Ordering::SeqCst);
    }

    pub(super) fn resume(&self) {
        *self.resumed.lock().unwrap() = Some(Instant::now());
    }
}

impl BenchmarkClock for MeasurementClock {
    fn now(&self) -> Instant {
        let elapsed = if self.expired.load(Ordering::SeqCst) {
            self.deadline
        } else {
            self.resumed
                .lock()
                .unwrap()
                .map(|started| started.elapsed().min(self.deadline))
                .unwrap_or_default()
        };
        self.origin + elapsed
    }
}

pub(super) struct AnalysisClock {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl AnalysisClock {
    pub(super) fn advance(session: &BenchmarkSession) -> Self {
        Self::spawn(session, None)
    }

    pub(super) fn advance_when_all_ready(session: &BenchmarkSession) -> Self {
        let files: Vec<String> = session
            .registered_files()
            .iter()
            .map(|row| row.file.file_id.to_string())
            .collect();
        Self::spawn(session, Some(files))
    }

    fn spawn(session: &BenchmarkSession, wait_for_all: Option<Vec<String>>) -> Self {
        let clock = Arc::new(api::probe_manager::clock::FakeClock::new(0));
        let manager = session.state.probe.clone();
        manager.set_clock(clock.clone());
        let store = session.state.store().clone();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut now = 0;
            let mut first_batch_released = wait_for_all.is_none();
            while !done.load(Ordering::SeqCst) {
                if !first_batch_released && let Some(files) = wait_for_all.as_ref() {
                    let all_in_analysis = !files.is_empty()
                        && files.iter().all(|file_id| {
                            store
                                .active_probe_unit_for_ledger(file_id)
                                .ok()
                                .flatten()
                                .is_some_and(|unit| {
                                    unit.stage == store::ProbeDispatchStage::Analysis
                                })
                        });
                    if all_in_analysis {
                        first_batch_released = true;
                    }
                }
                if first_batch_released
                    && !store
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
