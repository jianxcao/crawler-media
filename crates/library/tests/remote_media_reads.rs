#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use library::{Ffprobe, MediaProbe, extract_frame_with_ffmpeg};

fn program(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn metadata_and_artwork_processes_share_remote_admission() {
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
    let ready = Arc::new(Barrier::new(3));
    let metadata = {
        let (ready, media) = (ready.clone(), media.clone());
        std::thread::spawn(move || {
            ready.wait();
            Ffprobe::with_program(probe).probe(&media).unwrap()
        })
    };
    let artwork = {
        let ready = ready.clone();
        let out = dir.path().join("episode-thumb.jpg");
        std::thread::spawn(move || {
            ready.wait();
            extract_frame_with_ffmpeg(&media, 60_000, &out, frame.to_str().unwrap()).unwrap();
            std::fs::read(out).unwrap()
        })
    };
    ready.wait();
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
