#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use library::{
    DeliveryError, LibraryError, SubtitleRunner, SubtitleTrack, Tracks, WorkControl,
    deliver_subtitle_controlled,
};

struct RecordingRunner {
    inputs: Mutex<Vec<String>>,
}

impl RecordingRunner {
    fn new() -> Self {
        Self {
            inputs: Mutex::new(Vec::new()),
        }
    }

    fn inputs(&self) -> Vec<String> {
        self.inputs.lock().unwrap().clone()
    }
}

impl SubtitleRunner for RecordingRunner {
    fn output(
        &self,
        command: &mut Command,
        _control: &WorkControl,
    ) -> Result<Output, LibraryError> {
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let input_idx = args
            .iter()
            .position(|arg| arg == "-i")
            .expect("command must pass -i");
        let input = args
            .get(input_idx + 1)
            .expect("command must pass input after -i")
            .clone();
        let call_index = {
            let mut recorded = self.inputs.lock().unwrap();
            recorded.push(input);
            recorded.len()
        };
        if call_index == 1 {
            return Ok(Output {
                status: ExitStatus::from_raw(1 << 8),
                stdout: Vec::new(),
                stderr: b"HTTP error 403 Forbidden".to_vec(),
            });
        }
        let staged = PathBuf::from(args.last().expect("command must end with staging path"));
        std::fs::write(&staged, b"WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nok\n").unwrap();
        Ok(Output {
            status: ExitStatus::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
}

fn spawn_redirector() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let n = counter.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                line.clear();
            }
            let target = if n == 0 {
                "http://cdn.invalid/first-target.mkv"
            } else {
                "http://cdn.invalid/second-target.mkv"
            };
            let _ = write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: {target}\r\nCache-Control: max-age=300\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    (format!("http://{addr}/stream.strm"), hits)
}

#[test]
fn failed_strm_subtitle_extraction_invalidates_redirect_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let (redirector_url, hits) = spawn_redirector();
    let strm = tmp.path().join("movie.strm");
    std::fs::write(&strm, format!("{redirector_url}\n")).unwrap();
    let cache = tmp.path().join("cache");
    let tracks = Tracks {
        subtitles: vec![SubtitleTrack {
            stream_index: Some(2),
            codec: Some("subrip".into()),
            is_external: false,
            path: None,
            ..Default::default()
        }],
        ..Default::default()
    };
    let runner = RecordingRunner::new();

    let first = deliver_subtitle_controlled(
        &tracks,
        2,
        true,
        Some(&strm),
        Some(&cache),
        &WorkControl::default(),
        &runner,
    );
    assert!(matches!(first, Err(DeliveryError::Extraction(_))));

    let second = deliver_subtitle_controlled(
        &tracks,
        2,
        true,
        Some(&strm),
        Some(&cache),
        &WorkControl::default(),
        &runner,
    )
    .expect("re-resolved redirect target should succeed");

    assert_eq!(second.content_type, "text/vtt; charset=utf-8");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(
        runner.inputs(),
        vec![
            "http://cdn.invalid/first-target.mkv".to_string(),
            "http://cdn.invalid/second-target.mkv".to_string(),
        ]
    );
}
