use super::*;
pub struct Sample {
    pub outcome: &'static str,
    pub writer_wait_ns: u64,
    pub writer_hold_ns: u64,
    pub covered_owned_write_tx_count: u64,
    pub covered_owned_write_tx_ns: u64,
}
#[derive(Default)]
pub struct Report {
    pub samples: Vec<Sample>,
    pub calls_ms: Vec<f64>,
    pub files: usize,
}
pub struct RendezvousState {
    arrived: usize,
    aborted: bool,
}

pub struct BoundedRendezvous {
    parties: usize,
    state: std::sync::Mutex<RendezvousState>,
    changed: std::sync::Condvar,
}

pub struct RendezvousGuard {
    rendezvous: Arc<BoundedRendezvous>,
    armed: bool,
}

impl RendezvousGuard {
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RendezvousGuard {
    fn drop(&mut self) {
        if self.armed {
            let mut guard = self
                .rendezvous
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            guard.aborted = true;
            self.rendezvous.changed.notify_all();
        }
    }
}

impl BoundedRendezvous {
    pub fn new(parties: usize) -> Arc<Self> {
        Arc::new(Self {
            parties,
            state: std::sync::Mutex::new(RendezvousState {
                arrived: 0,
                aborted: false,
            }),
            changed: std::sync::Condvar::new(),
        })
    }

    pub fn guard(self: &Arc<Self>) -> RendezvousGuard {
        RendezvousGuard {
            rendezvous: self.clone(),
            armed: true,
        }
    }

    pub fn wait(&self) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.aborted {
            drop(guard);
            panic!("rendezvous aborted");
        }
        guard.arrived += 1;
        if guard.arrived >= self.parties {
            self.changed.notify_all();
            return;
        }
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while guard.arrived < self.parties && !guard.aborted {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                guard.aborted = true;
                self.changed.notify_all();
                drop(guard);
                panic!("rendezvous timed out");
            };
            let (next, timeout) = self
                .changed
                .wait_timeout(guard, remaining)
                .unwrap_or_else(|e| e.into_inner());
            guard = next;
            if timeout.timed_out() && guard.arrived < self.parties && !guard.aborted {
                guard.aborted = true;
                self.changed.notify_all();
                drop(guard);
                panic!("rendezvous timed out");
            }
        }
        if guard.aborted {
            drop(guard);
            panic!("rendezvous aborted");
        }
    }
}

pub fn run(
    store: Arc<store::Store>,
    templates: Vec<domain::LibraryFile>,
    root: std::path::PathBuf,
    scope: store::library_write::WriteScope,
    config: (bool, bool, usize),
    start: Arc<BoundedRendezvous>,
) -> Report {
    let (scan, dispersed, phase) = config;
    let samples = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = samples.clone();
    let mut report = Report::default();
    let mut guard = start.guard();
    start.wait();
    guard.disarm();
    store.with_writer_diagnostics(
        move |s| {
            if s.database == store::Db::Library {
                observed.borrow_mut().push(Sample {
                    outcome: s.outcome,
                    writer_wait_ns: s.writer_wait_ns,
                    writer_hold_ns: s.writer_hold_ns,
                    covered_owned_write_tx_count: s.covered_owned_write_tx_count,
                    covered_owned_write_tx_ns: s.covered_owned_write_tx_ns,
                });
            }
        },
        || {
            for round in 0..20 {
                let files: Vec<_> = (0..100)
                    .map(|n| {
                        let mut f = templates[if dispersed { n } else { 0 }].clone();
                        f.file_id = domain::LedgerId::new();
                        f.path = root.join(format!("p{phase}-r{round}-f{n}.mkv"));
                        f
                    })
                    .collect();
                let units: Vec<_> = (0..100)
                    .map(|n| [domain::UnitKey::new(1, 1 + (round * 100 + n))])
                    .collect();
                let began = Instant::now();
                if scan {
                    let rows: Vec<_> = files
                        .iter()
                        .zip(&units)
                        .map(|(f, u)| store::ScannedLedger {
                            file_id: f.file_id,
                            media_id: f.media_id,
                            path: f.path.display().to_string(),
                            units: u.to_vec(),
                            resolution: f.resolution.clone(),
                            codec: f.codec.clone(),
                            hdr: None,
                            quality_source: f.quality_source,
                            confidence: f.confidence,
                            filter_score: None,
                            source_version: f.version.source_version.clone(),
                            source_digest: None,
                            size_bytes: Some(1000),
                            created_ns: None,
                            modified_ns: None,
                            artwork: Default::default(),
                            observations: vec![],
                        })
                        .collect();
                    store
                        .insert_scanned_files(&rows, BASE + phase as i64)
                        .unwrap();
                } else {
                    let publications: Vec<_> = files
                        .iter()
                        .zip(&units)
                        .map(|(f, u)| publication(f, u))
                        .collect();
                    store
                        .publish_files(&scope, &publications, BASE + phase as i64)
                        .unwrap();
                }
                report.calls_ms.push(began.elapsed().as_secs_f64() * 1000.);
                report.files += files.len();
            }
        },
    );
    report.samples = std::mem::take(&mut *samples.borrow_mut());
    report
}
pub fn distribution(label: &str, mut values: Vec<f64>) {
    values.sort_by(f64::total_cmp);
    assert!(!values.is_empty());
    let p = |n: usize| values[(values.len() - 1) * n / 100];
    eprintln!(
        "L1 {label} n={} p50_ms={:.3} p95_ms={:.3} max_ms={:.3} gt500={} gt1000={}",
        values.len(),
        p(50),
        p(95),
        p(100),
        values.iter().filter(|v| **v > 500.).count(),
        values.iter().filter(|v| **v > 1000.).count()
    );
}
pub fn summarize(report: &Report, label: &str) {
    distribution(&format!("{label} call_wall"), report.calls_ms.clone());
    for (name, field) in [
        ("writer_wait", 0),
        ("writer_hold", 1),
        ("covered_owned_write_tx", 2),
    ] {
        distribution(&format!("{label} {name}"),report.samples.iter().map(|s|match field{0=>s.writer_wait_ns,1=>s.writer_hold_ns,_=>s.covered_owned_write_tx_ns} as f64/1e6).collect());
    }
    eprintln!(
        "L1 {label} files={} throughput_files_s={:.1} covered_owned_tx_count={}",
        report.files,
        report.files as f64 / (report.calls_ms.iter().sum::<f64>() / 1000.),
        report
            .samples
            .iter()
            .map(|s| s.covered_owned_write_tx_count)
            .sum::<u64>()
    );
}
