use library::subtitles::{find_subtitle_by_index, srt_to_vtt, subtitle_index};
use library::{AudioTrack, SubtitleTrack, Tracks, VideoTrack};

#[test]
fn srt_converts_to_vtt_with_comma_to_dot_timestamps() {
    let srt = "1\n00:00:01,000 --> 00:00:02,000\nHello World\n";
    let vtt = srt_to_vtt(srt);
    assert!(vtt.starts_with("WEBVTT\n\n"));
    assert!(vtt.contains("00:00:01.000 --> 00:00:02.000"));
    assert!(vtt.contains("Hello World"));
}

#[test]
fn subtitle_index_allocates_stable_unique_indexes() {
    let tracks = Tracks {
        video: Some(VideoTrack {
            stream_index: Some(0),
            ..Default::default()
        }),
        audio: vec![AudioTrack {
            stream_index: Some(1),
            ..Default::default()
        }],
        subtitles: vec![
            SubtitleTrack {
                stream_index: Some(2),
                codec: Some("subrip".into()),
                is_external: false,
                path: None,
                ..Default::default()
            },
            SubtitleTrack {
                stream_index: None,
                codec: Some("srt".into()),
                is_external: true,
                path: Some("/path/to/sub1.srt".into()),
                ..Default::default()
            },
            SubtitleTrack {
                stream_index: None,
                codec: Some("ass".into()),
                is_external: true,
                path: Some("/path/to/sub2.ass".into()),
                ..Default::default()
            },
        ],
    };

    assert_eq!(subtitle_index(&tracks, 0), 2);
    assert_eq!(subtitle_index(&tracks, 1), 3);
    assert_eq!(subtitle_index(&tracks, 2), 4);

    let found0 = find_subtitle_by_index(&tracks, 2).unwrap();
    assert_eq!(found0.codec.as_deref(), Some("subrip"));

    let found1 = find_subtitle_by_index(&tracks, 3).unwrap();
    assert_eq!(found1.path.as_deref(), Some("/path/to/sub1.srt"));

    let found2 = find_subtitle_by_index(&tracks, 4).unwrap();
    assert_eq!(found2.path.as_deref(), Some("/path/to/sub2.ass"));
}
