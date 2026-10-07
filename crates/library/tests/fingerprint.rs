use library::extract_frame;
use library::fingerprint::{extract_audio_fingerprint, find_common_segment};
use rusty_chromaprint::{Configuration, Fingerprinter};
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

static TEST_MUTEX: Mutex<()> = Mutex::new(());

fn recording_ffmpeg(tmp: &tempfile::TempDir) -> std::path::PathBuf {
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let ffmpeg = bin.join("ffmpeg");
    std::fs::write(
        &ffmpeg,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$FFMPEG_ARGS\"\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[test]
fn remote_strm_timeout_precedes_ffmpeg_input() {
    let _guard = TEST_MUTEX.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let bin = recording_ffmpeg(&tmp);
    let args_file = tmp.path().join("args");
    let strm = tmp.path().join("episode.strm");
    std::fs::write(&strm, "https://cdn.example.test/episode.mkv\n").unwrap();
    let old_path = std::env::var_os("PATH");
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        );
        std::env::set_var("FFMPEG_ARGS", &args_file);
    }

    let _ = extract_audio_fingerprint(&strm, 60);

    if let Some(path) = old_path {
        unsafe { std::env::set_var("PATH", path) };
    }
    unsafe { std::env::remove_var("FFMPEG_ARGS") };
    let args = std::fs::read_to_string(args_file).unwrap();
    let args: Vec<_> = args.lines().collect();
    let timeout = args.iter().position(|arg| *arg == "-timeout").unwrap();
    let input = args.iter().position(|arg| *arg == "-i").unwrap();
    assert!(
        timeout < input,
        "remote input options must precede -i: {args:?}"
    );
}

#[test]
fn remote_strm_timeout_precedes_frame_input() {
    let _guard = TEST_MUTEX.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let bin = recording_ffmpeg(&tmp);
    let args_file = tmp.path().join("frame-args");
    let strm = tmp.path().join("episode.strm");
    std::fs::write(&strm, "https://cdn.example.test/episode.mkv\n").unwrap();
    let old_path = std::env::var_os("PATH");
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        );
        std::env::set_var("FFMPEG_ARGS", &args_file);
    }

    extract_frame(&strm, 1_000, &tmp.path().join("frame.jpg")).unwrap();

    if let Some(path) = old_path {
        unsafe { std::env::set_var("PATH", path) };
    }
    unsafe { std::env::remove_var("FFMPEG_ARGS") };
    let args = std::fs::read_to_string(args_file).unwrap();
    let args: Vec<_> = args.lines().collect();
    let timeout = args.iter().position(|arg| *arg == "-timeout").unwrap();
    let input = args.iter().position(|arg| *arg == "-i").unwrap();
    assert!(
        timeout < input,
        "remote input options must precede -i: {args:?}"
    );
}

#[test]
fn test_chromaprint_matching_synthesized_signal() {
    let config = Configuration::preset_test2();
    let mut fp1 = Fingerprinter::new(&config);
    fp1.start(16000, 1).unwrap();

    let mut fp2 = Fingerprinter::new(&config);
    fp2.start(16000, 1).unwrap();

    // Synthesize 40 seconds of 440Hz sine wave (common intro)
    let mut common_samples = Vec::new();
    for i in 0..(16000 * 40) {
        let sample =
            (f32::sin(2.0 * std::f32::consts::PI * 440.0 * (i as f32) / 16000.0) * 10000.0) as i16;
        common_samples.push(sample);
    }

    // Ep 1: 10s silence, then 40s intro, then 20s different
    let mut ep1_samples = vec![0i16; 16000 * 10];
    ep1_samples.extend_from_slice(&common_samples);
    for i in 0..(16000 * 20) {
        let sample =
            (f32::sin(2.0 * std::f32::consts::PI * 880.0 * (i as f32) / 16000.0) * 8000.0) as i16;
        ep1_samples.push(sample);
    }

    // Ep 2: 5s silence, then 40s intro, then 20s different
    let mut ep2_samples = vec![0i16; 16000 * 5];
    ep2_samples.extend_from_slice(&common_samples);
    for i in 0..(16000 * 20) {
        let sample =
            (f32::sin(2.0 * std::f32::consts::PI * 1200.0 * (i as f32) / 16000.0) * 8000.0) as i16;
        ep2_samples.push(sample);
    }

    fp1.consume(&ep1_samples);
    fp1.finish();

    fp2.consume(&ep2_samples);
    fp2.finish();

    let f1 = fp1.fingerprint();
    let f2 = fp2.fingerprint();

    assert!(!f1.is_empty());
    assert!(!f2.is_empty());

    let matched = find_common_segment(f1, f2, 15.0, 60.0);
    assert!(matched.is_some(), "Should find the 40s common segment");
    let seg = matched.unwrap();
    assert!(
        seg.duration_sec >= 30.0,
        "Matched duration should be close to 40s"
    );
}
