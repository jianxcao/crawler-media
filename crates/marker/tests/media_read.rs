use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use marker::{WorkControl, media_read::MediaReadGate};

struct RendezvousState {
    arrived: usize,
    aborted: bool,
}

struct BoundedRendezvous {
    parties: usize,
    state: Mutex<RendezvousState>,
    changed: Condvar,
}

struct RendezvousGuard {
    rendezvous: Arc<BoundedRendezvous>,
    armed: bool,
}

impl RendezvousGuard {
    fn disarm(&mut self) {
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
    fn new(parties: usize) -> Arc<Self> {
        Arc::new(Self {
            parties,
            state: Mutex::new(RendezvousState {
                arrived: 0,
                aborted: false,
            }),
            changed: Condvar::new(),
        })
    }

    fn guard(self: &Arc<Self>) -> RendezvousGuard {
        RendezvousGuard {
            rendezvous: self.clone(),
            armed: true,
        }
    }

    fn wait(&self) {
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
        let deadline = Instant::now() + Duration::from_secs(10);
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

fn remote(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("episode.strm");
    std::fs::write(&path, "https://fixture.invalid/episode.mkv\n").unwrap();
    path
}

#[test]
fn metadata_and_fingerprint_reads_share_exclusion_and_cooldown() {
    let dir = tempfile::tempdir().unwrap();
    let path = remote(dir.path());
    let cooldown = Duration::from_millis(40);
    let gate = Arc::new(MediaReadGate::new(1, cooldown));
    let ready = BoundedRendezvous::new(3);
    let (tx, rx) = mpsc::channel();
    let workers: Vec<_> = ["metadata", "fingerprint"]
        .into_iter()
        .map(|stage| {
            let (gate, ready, tx, path) = (gate.clone(), ready.clone(), tx.clone(), path.clone());
            std::thread::spawn(move || {
                let mut guard = ready.guard();
                ready.wait();
                guard.disarm();
                gate.read(&path, &WorkControl::default(), || {
                    tx.send((stage, "start", Instant::now())).unwrap();
                    std::thread::sleep(Duration::from_millis(30));
                    tx.send((stage, "finish", Instant::now())).unwrap();
                    Ok(())
                })
                .unwrap();
            })
        })
        .collect();
    let mut main_guard = ready.guard();
    ready.wait();
    main_guard.disarm();
    for worker in workers {
        worker.join().unwrap();
    }
    let events: Vec<_> = rx.try_iter().collect();
    assert_eq!(
        events
            .iter()
            .map(|(_, event, _)| *event)
            .collect::<Vec<_>>(),
        ["start", "finish", "start", "finish"]
    );
    assert!(events[2].2.duration_since(events[1].2) >= cooldown);
}

#[test]
fn cancelled_waiter_does_not_read_or_block_next_episode() {
    let dir = tempfile::tempdir().unwrap();
    let path = remote(dir.path());
    let gate = Arc::new(MediaReadGate::new(1, Duration::ZERO));
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = {
        let (gate, path) = (gate.clone(), path.clone());
        std::thread::spawn(move || {
            gate.read(&path, &WorkControl::default(), || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
        })
    };
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let control = WorkControl::default();
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let waiter = {
        let (gate, path, control) = (gate.clone(), path.clone(), control.clone());
        std::thread::spawn(move || {
            waiting_tx.send(()).unwrap();
            let result = gate.read::<()>(&path, &control, || panic!("cancelled read started"));
            result_tx.send(result).unwrap();
        })
    };
    waiting_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    control.cancel();
    assert!(
        result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_err()
    );
    waiter.join().unwrap();
    release_tx.send(()).unwrap();
    running.join().unwrap().unwrap();
    assert_eq!(
        gate.read(&path, &WorkControl::default(), || Ok(42))
            .unwrap(),
        42
    );
}

#[test]
fn local_reads_are_not_serialized_behind_remote_episode() {
    let dir = tempfile::tempdir().unwrap();
    let path = remote(dir.path());
    let gate = Arc::new(MediaReadGate::new(1, Duration::from_secs(5)));
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = {
        let gate = gate.clone();
        std::thread::spawn(move || {
            gate.read(&path, &WorkControl::default(), || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
        })
    };
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(
        gate.read(Path::new("episode.mkv"), &WorkControl::default(), || Ok(7))
            .unwrap(),
        7
    );
    release_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
}

#[test]
fn failed_remote_read_still_cools_down_and_releases_admission() {
    let dir = tempfile::tempdir().unwrap();
    let path = remote(dir.path());
    let cooldown = Duration::from_millis(40);
    let gate = MediaReadGate::new(1, cooldown);
    assert!(
        gate.read::<()>(&path, &WorkControl::default(), || Err("HTTP 500".into()))
            .is_err()
    );
    let failed_at = Instant::now();
    gate.read(&path, &WorkControl::default(), || {
        assert!(failed_at.elapsed() >= cooldown);
        Ok(())
    })
    .unwrap();
}

#[test]
fn gate_admits_up_to_its_limit_of_concurrent_remote_reads() {
    let dir = tempfile::tempdir().unwrap();
    let path = remote(dir.path());
    let gate = Arc::new(MediaReadGate::new(2, Duration::ZERO));
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    let workers: Vec<_> = (0..3)
        .map(|n| {
            let (gate, path, started_tx, release_rx) = (
                gate.clone(),
                path.clone(),
                started_tx.clone(),
                release_rx.clone(),
            );
            std::thread::spawn(move || {
                gate.read(&path, &WorkControl::default(), || {
                    started_tx.send(n).unwrap();
                    release_rx.lock().unwrap().recv().unwrap();
                    Ok(())
                })
                .unwrap();
            })
        })
        .collect();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        started_rx.recv_timeout(Duration::from_millis(100)).is_err(),
        "third read must wait for a free slot"
    );
    release_tx.send(()).unwrap();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    release_tx.send(()).unwrap();
    release_tx.send(()).unwrap();
    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
fn gate_limit_can_be_raised_at_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = remote(dir.path());
    let gate = Arc::new(MediaReadGate::new(1, Duration::ZERO));
    gate.configure(3, Duration::ZERO);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    let workers: Vec<_> = (0..3)
        .map(|_| {
            let (gate, path, started_tx, release_rx) = (
                gate.clone(),
                path.clone(),
                started_tx.clone(),
                release_rx.clone(),
            );
            std::thread::spawn(move || {
                gate.read(&path, &WorkControl::default(), || {
                    started_tx.send(()).unwrap();
                    release_rx.lock().unwrap().recv().unwrap();
                    Ok(())
                })
                .unwrap();
            })
        })
        .collect();
    for _ in 0..3 {
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    for _ in 0..3 {
        release_tx.send(()).unwrap();
    }
    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
fn changing_cooldown_updates_waiting_reads_without_restarting_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    let path = remote(dir.path());
    let gate = MediaReadGate::new(1, Duration::from_secs(5));
    gate.read(&path, &WorkControl::default(), || Ok(()))
        .unwrap();
    gate.configure(1, Duration::ZERO);
    let control = WorkControl::default();
    let cancel = control.clone();
    let watchdog = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        cancel.cancel();
    });
    let result = gate.read(&path, &control, || Ok(42));
    watchdog.join().unwrap();
    assert_eq!(result.unwrap(), 42);
}

#[test]
fn pausing_remote_reads_releases_waiters_without_interrupting_active_reads() {
    let directory = tempfile::tempdir().unwrap();
    let active_path = remote(directory.path()).with_file_name("active.strm");
    let waiting_path = remote(directory.path()).with_file_name("waiting.strm");
    let active =
        marker::media_read::acquire_media_read(&active_path, &WorkControl::default()).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = marker::media_read::acquire_media_read(&waiting_path, &WorkControl::default())
            .map(|_| ());
        done_tx.send(result).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    std::thread::sleep(Duration::from_millis(25));
    marker::media_read::set_remote_reads_paused(true);
    let result = done_rx.recv_timeout(Duration::from_millis(100));
    drop(active);
    marker::media_read::set_remote_reads_paused(false);
    waiter.join().unwrap();
    assert_eq!(
        result
            .expect("pause must release the blocked gate waiter")
            .unwrap_err(),
        marker::media_read::READ_WAIT_STOPPED
    );
}

#[test]
fn direct_chapter_probe_waits_for_shared_remote_admission_and_cancels() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local-only.strm");
    // An invalid URI exercises STRM admission without a valid network target,
    // even in the regression's ungated implementation.
    std::fs::write(&path, "http://[invalid/chapter-fixture.mkv\n").unwrap();
    let permit = marker::media_read::acquire_media_read(&path, &WorkControl::default()).unwrap();
    let control = WorkControl::default();
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = {
        let control = control.clone();
        std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx
                .send(marker::probe_chapters_controlled(&path, &control))
                .unwrap();
        })
    };
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let premature = done_rx.recv_timeout(Duration::from_millis(500));
    control.cancel();
    let was_waiting = matches!(premature, Err(mpsc::RecvTimeoutError::Timeout));
    if was_waiting {
        assert!(
            done_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_none()
        );
    }
    worker.join().unwrap();
    drop(permit);
    assert!(
        was_waiting,
        "chapter ffprobe bypassed the shared remote read permit"
    );
}
