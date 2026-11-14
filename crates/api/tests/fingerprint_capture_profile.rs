use api::fingerprint_job::{
    analysis_policy_key, capture_profile_key, capture_source, profile_for_source,
};

fn track(index: u32, default: bool) -> library::AudioTrack {
    library::AudioTrack {
        stream_index: Some(index),
        is_default: default,
        capture_source: Some(marker::CaptureSourceFacts {
            audio_stream_index: index,
            format_start_time_us: None,
            stream_start_pts: Some(400),
            stream_time_base: Some("1/48000".into()),
            codec_name: None,
            native_sample_rate: None,
            audio_channels: None,
            provenance: "fixture_header".into(),
        }),
        ..Default::default()
    }
}

#[test]
fn selection_is_explicit_then_default_then_lowest_global_audio_index() {
    let mut tracks = library::Tracks {
        audio: vec![track(4, false), track(2, true), track(1, false)],
        ..Default::default()
    };
    assert_eq!(capture_source(&tracks, None).unwrap().audio_stream_index, 2);
    assert_eq!(
        capture_source(&tracks, Some(4)).unwrap().audio_stream_index,
        4
    );
    assert!(capture_source(&tracks, Some(0)).is_err());
    tracks.audio[1].is_default = false;
    assert_eq!(capture_source(&tracks, None).unwrap().audio_stream_index, 1);
    tracks.audio[2].capture_source = None;
    assert!(capture_source(&tracks, None).is_err());
    tracks.audio[2] = track(1, false);
    tracks.audio[2]
        .capture_source
        .as_mut()
        .unwrap()
        .audio_stream_index = 5;
    assert!(capture_source(&tracks, Some(1)).is_err());
}

#[test]
fn analysis_thresholds_do_not_change_captures_but_selected_audio_does() {
    let source = track(1, true).capture_source.unwrap();
    let profile = profile_for_source(Some(&source));
    let first = marker::SamplingPolicy::default();
    let mut second = first.clone();
    second.max_score += 1.;
    assert_ne!(analysis_policy_key(&first), analysis_policy_key(&second));
    assert_eq!(
        capture_profile_key(&profile),
        capture_profile_key(&profile_for_source(Some(&source)))
    );
    let mut other = source;
    other.audio_stream_index = 2;
    assert_ne!(
        capture_profile_key(&profile),
        capture_profile_key(&profile_for_source(Some(&other)))
    );
}

#[test]
fn quantized_aac_mapping_explicitly_invalidates_old_capture_contract() {
    let mut source = track(1, true).capture_source.unwrap();
    source.codec_name = Some("aac".into());
    source.native_sample_rate = Some(44_100);
    source.audio_channels = Some(2);
    source.stream_time_base = Some("1/1000".into());
    let current = profile_for_source(Some(&source));
    assert_eq!(current.time_mapping_version, 4);
    let mut old = current.clone();
    old.time_mapping_version = 3;
    assert_ne!(capture_profile_key(&current), capture_profile_key(&old));
    source.native_sample_rate = Some(48_000);
    let supported48 = profile_for_source(Some(&source));
    assert_eq!(supported48.time_mapping_version, 4);
    let mut old48 = supported48.clone();
    old48.time_mapping_version = 3;
    assert_ne!(
        capture_profile_key(&supported48),
        capture_profile_key(&old48)
    );
    source.audio_channels = Some(1);
    assert_eq!(
        profile_for_source(Some(&source)).time_mapping_version,
        3,
        "mono is outside the proved stereo subset"
    );
    source.audio_channels = Some(2);
    source.stream_time_base = Some("1/48000".into());
    assert_eq!(
        profile_for_source(Some(&source)).time_mapping_version,
        3,
        "fine clock retains its contract"
    );
}
