#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use library::{Ffprobe, MediaProbe, extract_frame_with_ffmpeg, probe_duration_with_ffprobe};

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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while guard.arrived < self.parties && !guard.aborted {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
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

fn program(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn metadata_and_artwork_processes_share_remote_admission() {
    // Limit 1 makes sharing observable: both paths take the same gate.
    marker::media_read::configure_remote_reads(1, std::time::Duration::ZERO);
    let dir = tempfile::tempdir().unwrap();
    let events = dir.path().join("events");
    let probe = program(
        dir.path(),
        "probe",
        &format!(
            "for input do :; done\n[ \"$input\" = https://fixture.invalid/video ] || exit 9\necho metadata-start >> '{}'\nsleep 0.1\necho metadata-finish >> '{}'\nprintf '%s' '{{\"streams\":[{{\"codec_name\":\"hevc\",\"width\":1920,\"height\":1080}}]}}'",
            events.display(),
            events.display()
        ),
    );
    let frame = program(
        dir.path(),
        "frame",
        &format!(
            "echo artwork-start >> '{}'\nsleep 0.1\nfor out do :; done\nprintf jpeg > \"$out\"\necho artwork-finish >> '{}'",
            events.display(),
            events.display()
        ),
    );
    let media = dir.path().join("episode.strm");
    std::fs::write(&media, "https://fixture.invalid/video\n").unwrap();
    let ready = BoundedRendezvous::new(3);
    let metadata = {
        let (ready, media) = (ready.clone(), media.clone());
        std::thread::spawn(move || {
            let mut guard = ready.guard();
            ready.wait();
            guard.disarm();
            Ffprobe::with_program(probe).probe(&media).unwrap()
        })
    };
    let artwork = {
        let ready = ready.clone();
        let out = dir.path().join("episode-thumb.jpg");
        std::thread::spawn(move || {
            let mut guard = ready.guard();
            ready.wait();
            guard.disarm();
            extract_frame_with_ffmpeg(&media, 60_000, &out, frame.to_str().unwrap()).unwrap();
            std::fs::read(out).unwrap()
        })
    };
    let mut main_guard = ready.guard();
    ready.wait();
    main_guard.disarm();
    assert_eq!(metadata.join().unwrap().codec.as_deref(), Some("hevc"));
    assert_eq!(artwork.join().unwrap(), b"jpeg");
    let events = std::fs::read_to_string(events).unwrap();
    let events = events.lines().collect::<Vec<_>>();
    assert!(
        events
            == [
                "metadata-start",
                "metadata-finish",
                "artwork-start",
                "artwork-finish"
            ]
            || events
                == [
                    "artwork-start",
                    "artwork-finish",
                    "metadata-start",
                    "metadata-finish"
                ],
        "remote media processes overlapped: {events:?}"
    );
}

#[test]
fn duration_probe_shares_remote_admission_with_artwork() {
    marker::media_read::configure_remote_reads(1, std::time::Duration::ZERO);
    let dir = tempfile::tempdir().unwrap();
    let events = dir.path().join("events");
    let ffprobe_dur = program(
        dir.path(),
        "ffprobe-dur",
        &format!(
            "echo duration-start >> '{}'\nsleep 0.15\necho duration-finish >> '{}'\nprintf '%s' '{{\"format\":{{\"duration\":\"120.5\"}}}}'",
            events.display(),
            events.display()
        ),
    );
    let frame = program(
        dir.path(),
        "frame",
        &format!(
            "echo artwork-start >> '{}'\nsleep 0.15\nfor out do :; done\nprintf jpeg > \"$out\"\necho artwork-finish >> '{}'",
            events.display(),
            events.display()
        ),
    );
    let media = dir.path().join("episode.strm");
    std::fs::write(&media, "https://fixture.invalid/video\n").unwrap();
    let ready = BoundedRendezvous::new(3);
    let duration = {
        let (ready, media, ffprobe_dur) = (ready.clone(), media.clone(), ffprobe_dur.clone());
        std::thread::spawn(move || {
            let mut guard = ready.guard();
            ready.wait();
            guard.disarm();
            probe_duration_with_ffprobe(&media, ffprobe_dur.to_str().unwrap())
        })
    };
    let artwork = {
        let ready = ready.clone();
        let out = dir.path().join("episode-thumb.jpg");
        std::thread::spawn(move || {
            let mut guard = ready.guard();
            ready.wait();
            guard.disarm();
            extract_frame_with_ffmpeg(&media, 60_000, &out, frame.to_str().unwrap()).unwrap();
            std::fs::read(out).unwrap()
        })
    };
    let mut main_guard = ready.guard();
    ready.wait();
    main_guard.disarm();
    assert_eq!(duration.join().unwrap(), Some(120_500));
    assert_eq!(artwork.join().unwrap(), b"jpeg");
    let events = std::fs::read_to_string(events).unwrap();
    let events = events.lines().collect::<Vec<_>>();
    assert!(
        events
            == [
                "duration-start",
                "duration-finish",
                "artwork-start",
                "artwork-finish"
            ]
            || events
                == [
                    "artwork-start",
                    "artwork-finish",
                    "duration-start",
                    "duration-finish"
                ],
        "duration probe overlapped remote artwork extraction: {events:?}"
    );
}
