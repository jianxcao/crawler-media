#[cfg(unix)]
#[test]
fn ffprobe_maps_video_stream_to_library_quality() {
    use std::os::unix::fs::PermissionsExt;

    use library::{Ffprobe, MediaProbe};

    let tmp = tempfile::tempdir().unwrap();
    let program = tmp.path().join("ffprobe");
    std::fs::write(
        &program,
        r#"#!/bin/sh
cat <<'JSON'
{"streams":[{"codec_name":"hevc","width":3840,"height":2160,"color_transfer":"smpte2084","side_data_list":[]}]}
JSON
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&program).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&program, permissions).unwrap();
    seal_executable(&program);
    let media = tmp.path().join("movie.mkv");
    std::fs::write(&media, b"fixture").unwrap();

    let quality = Ffprobe::with_program(program).probe(&media).unwrap();

    assert_eq!(quality.resolution.as_deref(), Some("2160p"));
    assert_eq!(quality.codec.as_deref(), Some("hevc"));
    assert_eq!(quality.hdr.as_deref(), Some("HDR10"));
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
