//! A heartbeat cannot rebind omitted track selections to a different source or track revision.
#[path = "playback_event_support/mod.rs"]
mod playback_event_support;
use playback_event_support::*;
use store::library_write::probe::{Observed, ProbePublication};
use store::playback::ResolvedResume;

fn progress(
    f: &Fixture,
    file: &LibraryFile,
    tracks: (Option<&str>, Option<&str>),
) -> ResolvedResume {
    let item = episode(f, 1);
    let session = issue(f, &item, file);
    let mut bound = event(
        f,
        item.clone(),
        (&session.0, session.1),
        file,
        9_000,
        PlaybackEventType::Progress,
    );
    bound.audio_track = tracks.0.map(Into::into);
    bound.subtitle_track = tracks.1.map(Into::into);
    assert_eq!(
        f.store.commit_playback_event(&bound, 2_000).unwrap(),
        EventOutcome::Applied
    );
    resolve(f, file)
}

fn resolve(f: &Fixture, file: &LibraryFile) -> ResolvedResume {
    f.store
        .resolve_playback(
            USER.parse().unwrap(),
            &episode(f, 1),
            Some(&PhysicalSourceId {
                file_id: file.file_id,
                revision: file.version.revision,
            }),
            None,
        )
        .unwrap()
}

fn publish(f: &Fixture, file: &LibraryFile) {
    f.store
        .publish_files(
            &WriteScope {
                library_id: f.library_id.parse().unwrap(),
                config_revision: 0,
            },
            &[StorePublication {
                file,
                units: &[UnitKey::new(1, 1)],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            3_000,
        )
        .unwrap();
}

#[test]
fn omitted_tracks_stay_cleared_after_progress_on_another_source() {
    for same_file in [false, true] {
        let (_tmp, f) = seed();
        let original = progress(&f, &f.file, (Some("embedded:3"), Some("embedded:7")));
        assert_eq!(original.audio_track.as_deref(), Some("embedded:3"));
        assert_eq!(original.subtitle_track.as_deref(), Some("embedded:7"));
        let mut replacement = f.file.clone();
        if same_file {
            replacement.version = FileVersion {
                revision: 2,
                source_version: "v2".into(),
            };
        } else {
            replacement.file_id = "00000000-0000-0000-0000-000000000012".parse().unwrap();
            replacement.path = "Merged Show/replacement.mkv".into();
        }
        publish(&f, &replacement);
        let before = resolve(&f, &replacement);
        assert_eq!((before.audio_track, before.subtitle_track), (None, None));
        let after = progress(&f, &replacement, (None, None));
        assert_eq!(after.status, ResumeStatus::Compatible);
        assert_eq!(after.start_ms, 9_000);
        assert_eq!(
            (after.audio_track, after.subtitle_track),
            (None, None),
            "omitted tracks must not migrate to the newly bound source"
        );
    }
}

#[test]
fn omitted_tracks_stay_cleared_after_progress_on_refreshed_tracks() {
    let (_tmp, f) = seed();
    progress(&f, &f.file, (Some("embedded:3"), Some("embedded:7")));
    assert!(
        f.store
            .publish_probe_result(
                f.file.file_id,
                &ProbePublication {
                    expected: &f.file.version,
                    tracks_version: f.file.tracks_version + 1,
                    resolution: None,
                    codec: None,
                    hdr: Observed::NotObserved,
                    duration_ms: None,
                    details: None,
                },
                3_000
            )
            .unwrap()
    );
    let before = resolve(&f, &f.file);
    assert_eq!((before.audio_track, before.subtitle_track), (None, None));
    let after = progress(&f, &f.file, (None, None));
    assert_eq!(after.start_ms, 9_000);
    assert_eq!((after.audio_track, after.subtitle_track), (None, None));
}

#[test]
fn omitted_tracks_retain_selections_for_the_same_binding_and_track_revision() {
    let (_tmp, f) = seed();
    progress(&f, &f.file, (Some("embedded:3"), Some("embedded:7")));
    let after = progress(&f, &f.file, (None, None));
    assert_eq!(after.audio_track.as_deref(), Some("embedded:3"));
    assert_eq!(after.subtitle_track.as_deref(), Some("embedded:7"));
    let explicit = progress(&f, &f.file, (Some("embedded:4"), Some("off")));
    assert_eq!(explicit.audio_track.as_deref(), Some("embedded:4"));
    assert_eq!(explicit.subtitle_track.as_deref(), Some("off"));
}

#[test]
fn explicit_tracks_apply_to_a_new_binding_without_retaining_other_old_tracks() {
    let (_tmp, f) = seed();
    progress(&f, &f.file, (Some("embedded:3"), Some("embedded:7")));
    let mut replacement = f.file.clone();
    replacement.version = FileVersion {
        revision: 2,
        source_version: "v2".into(),
    };
    publish(&f, &replacement);
    let after = progress(&f, &replacement, (Some("embedded:4"), None));
    assert_eq!(after.audio_track.as_deref(), Some("embedded:4"));
    assert_eq!(after.subtitle_track, None);
    let explicit = progress(&f, &replacement, (None, Some("off")));
    assert_eq!(explicit.audio_track.as_deref(), Some("embedded:4"));
    assert_eq!(explicit.subtitle_track.as_deref(), Some("off"));
}
