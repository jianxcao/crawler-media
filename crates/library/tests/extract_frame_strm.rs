use library::extract_frame_with_ffmpeg;
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn fake_ffmpeg(dir: &std::path::Path) -> std::path::PathBuf {
    let program = dir.join("ffmpeg");
    fs::write(
        &program,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/argv.txt\"\nfor last; do :; done\n: > \"$last\"\nexit 0\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&program).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&program, permissions).unwrap();
    seal_executable(&program);
    program
}

#[test]
fn extract_frame_seeks_the_url_inside_a_strm_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg(tmp.path());
    let strm = tmp.path().join("episode.strm");
    fs::write(&strm, "https://cdn.example/ep.mkv\n").unwrap();
    let jpeg = tmp.path().join("episode-thumb.jpg");
    extract_frame_with_ffmpeg(&strm, 60_000, &jpeg, ffmpeg.to_str().unwrap()).unwrap();
    let argv = fs::read_to_string(tmp.path().join("argv.txt")).unwrap();
    let input_at = argv.find("-i\n").expect(&argv);
    let url_at = argv.find("https://cdn.example/ep.mkv").expect(&argv);
    let out_at = argv.find("episode-thumb.jpg").expect(&argv);
    assert!(input_at < url_at && url_at < out_at, "{argv}");
    assert!(argv.contains("-ss\n60"), "{argv}");
    assert!(jpeg.is_file());
}

#[test]
fn extract_frame_passes_a_local_file_after_input_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg(tmp.path());
    let video = tmp.path().join("episode.mkv");
    fs::write(&video, b"not a real video").unwrap();
    let jpeg = tmp.path().join("episode-thumb.jpg");
    extract_frame_with_ffmpeg(&video, 60_000, &jpeg, ffmpeg.to_str().unwrap()).unwrap();
    let argv = fs::read_to_string(tmp.path().join("argv.txt")).unwrap();
    let input_at = argv.find("-i\n").expect(&argv);
    let file_at = argv.find("episode.mkv").expect(&argv);
    let out_at = argv.find("episode-thumb.jpg").expect(&argv);
    assert!(input_at < file_at && file_at < out_at, "{argv}");
}

/// Re-create a freshly written test script from a child process before it is
/// exec'd. A sibling test thread's fork can inherit this process's (CLOEXEC)
/// write fd for the script inode until that child execs, making our exec fail
/// with ETXTBSY. Rename-into-place does not help (the fd pins the inode); a
/// copy made by `cp` gives a new inode this process never opened for writing.
#[cfg(unix)]
fn seal_executable(path: &std::path::Path) {
    let mut sealed = path.as_os_str().to_owned();
    sealed.push(".sealed");
    let status = std::process::Command::new("cp")
        .arg("-p")
        .arg(path)
        .arg(&sealed)
        .status()
        .expect("spawn cp to seal test executable");
    assert!(status.success(), "cp failed sealing {}", path.display());
    std::fs::rename(&sealed, path).expect("rename sealed test executable");
}
