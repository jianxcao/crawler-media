use super::probe_tracks_with_program;

#[cfg(unix)]
#[test]
fn probe_tracks_and_duration_returns_format_duration_in_the_same_probe() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let program = tmp.path().join("ffprobe-fixture");
    std::fs::write(
        &program,
        r##"#!/bin/sh
echo called >> "$0.calls"
cat <<'JSON'
{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080} ],"format":{"duration":"5400.125"}}
JSON
"##,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&program).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&program, permissions).unwrap();
    let media = tmp.path().join("episode.mkv");
    std::fs::write(&media, b"fixture").unwrap();

    let (tracks, duration_ms) = super::probe_tracks_and_duration_with_program(&media, &program)
        .expect("ffprobe fixture should return tracks and duration");

    assert!(tracks.video.is_some());
    assert_eq!(duration_ms, Some(5_400_125));
    let calls = std::fs::read_to_string(program.with_extension("calls")).unwrap();
    assert_eq!(
        calls.lines().count(),
        1,
        "duration must reuse the tracks probe"
    );
}

#[cfg(unix)]
#[test]
fn ffprobe_keeps_detailed_video_audio_and_subtitle_facts() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let program = tmp.path().join("ffprobe-fixture");
    std::fs::write(
        &program,
        r##"#!/bin/sh
cat <<'JSON'
{"streams":[{"index":0,"codec_type":"video","codec_name":"hevc","profile":"Main 10","width":3840,"height":1600,"r_frame_rate":"24/1","avg_frame_rate":"24000/1001","display_aspect_ratio":"12:5","sample_aspect_ratio":"1:1","pix_fmt":"yuv420p10le","color_space":"bt2020nc","color_transfer":"smpte2084","color_primaries":"bt2020","bit_rate":"18000000","disposition":{"default":1}},{"index":1,"codec_type":"audio","codec_name":"eac3","profile":"Dolby Digital Plus","channels":6,"channel_layout":"5.1(side)","sample_rate":"48000","bit_rate":"768000","tags":{"language":"ja","title":"Japanese 5.1"},"disposition":{"default":1}},{"index":2,"codec_type":"subtitle","codec_name":"ass","tags":{"language":"zh","title":"简体中文"},"disposition":{"forced":1}}],"format":{"duration":"7200"}}
JSON
"##,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&program).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&program, permissions).unwrap();
    let media = tmp.path().join("movie.mkv");
    std::fs::write(&media, b"fixture").unwrap();

    let tracks = probe_tracks_with_program(&media, &program).unwrap();
    let video = tracks.video.unwrap();
    assert!(video.is_default);
    assert_eq!(video.profile.as_deref(), Some("Main 10"));
    assert_eq!(video.bit_depth, Some(10));
    assert_eq!(video.aspect_ratio.as_deref(), Some("12:5"));
    assert_eq!(video.color_transfer.as_deref(), Some("smpte2084"));
    assert_eq!(video.average_frame_rate, Some(24000.0 / 1001.0));
    let audio = &tracks.audio[0];
    assert_eq!(audio.language.as_deref(), Some("ja"));
    assert_eq!(audio.title.as_deref(), Some("Japanese 5.1"));
    assert_eq!(audio.channel_layout.as_deref(), Some("5.1(side)"));
    assert_eq!(audio.bit_rate, Some(768000));
    let subtitle = &tracks.subtitles[0];
    assert_eq!(subtitle.title.as_deref(), Some("简体中文"));
    assert!(subtitle.forced);
}

#[test]
fn discovers_matching_external_subtitles_with_language_and_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("movie.mkv");
    std::fs::write(&media, b"fixture").unwrap();
    let subtitle = tmp.path().join("movie.en.forced.srt");
    std::fs::write(&subtitle, "subtitle").unwrap();
    std::fs::write(tmp.path().join("movie2.zh.srt"), "other").unwrap();

    let subtitles = super::super::external_subtitle_tracks(&media).unwrap();

    assert_eq!(subtitles.len(), 1);
    assert_eq!(subtitles[0].codec.as_deref(), Some("srt"));
    assert_eq!(subtitles[0].language.as_deref(), Some("en"));
    assert!(subtitles[0].forced);
    assert!(subtitles[0].is_external);
    assert_eq!(
        subtitles[0].path.as_deref(),
        Some(subtitle.to_str().unwrap())
    );
}
