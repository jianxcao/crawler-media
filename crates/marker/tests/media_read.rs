use std::path::Path;
use std::sync::{Arc, Barrier, mpsc};
use std::time::{Duration, Instant};

use marker::{WorkControl, media_read::MediaReadGate};

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
    let gate = Arc::new(MediaReadGate::new(cooldown));
    let ready = Arc::new(Barrier::new(3));
    let (tx, rx) = mpsc::channel();
    let workers: Vec<_> = ["metadata", "fingerprint"]
        .into_iter()
        .map(|stage| {
            let (gate, ready, tx, path) = (gate.clone(), ready.clone(), tx.clone(), path.clone());
            std::thread::spawn(move || {
                ready.wait();
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
    ready.wait();
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
    let gate = Arc::new(MediaReadGate::new(Duration::ZERO));
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
    let gate = Arc::new(MediaReadGate::new(Duration::from_secs(5)));
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
    let gate = MediaReadGate::new(cooldown);
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
