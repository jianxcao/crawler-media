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
    let media = tmp.path().join("movie.mkv");
    std::fs::write(&media, b"fixture").unwrap();

    let quality = Ffprobe::with_program(program).probe(&media).unwrap();

    assert_eq!(quality.resolution.as_deref(), Some("2160p"));
    assert_eq!(quality.codec.as_deref(), Some("hevc"));
    assert_eq!(quality.hdr.as_deref(), Some("HDR10"));
}
