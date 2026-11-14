use super::*;

#[test]
fn control_uniform_season_publishes_all_markers() {
    let f = setup("uniform", &[1, 2, 3, 4]);
    let replacement = run(&f);
    assert_eq!(
        replacement
            .markers
            .iter()
            .filter(|marker| marker.intro_start_ms.is_some())
            .count(),
        4
    );
    f.store
        .as_ref()
        .complete_marker_refresh("review9-job", &[replacement])
        .unwrap();
    assert_eq!(
        f.store
            .as_ref()
            .get_probe_job("review9-job")
            .unwrap()
            .unwrap()
            .status,
        "succeeded"
    );
}

#[test]
fn duplicate_episode_versions_publish_without_unique_key_failure() {
    let f = setup("uniform", &[1, 1, 2, 3]);
    let replacement = run(&f);
    println!(
        "multiple versions generated episode rows: {:?}",
        replacement
            .markers
            .iter()
            .map(|marker| marker.unit.episode)
            .collect::<Vec<_>>()
    );
    let result = f
        .store
        .as_ref()
        .complete_marker_refresh("review9-job", &[replacement]);
    println!("publication: {result:?}");
    assert!(
        result.is_ok(),
        "a valid season with multiple versions of an episode must be publishable"
    );
}

#[test]
fn full_samples_from_nonseeds_discover_the_common_intro() {
    let f = setup("blind-seeds", &[1, 2, 3, 4, 5, 6, 7, 8]);
    let known_intro: Vec<_> = f
        .units
        .iter()
        .map(|unit| {
            let episode = unit
                .row
                .units
                .iter()
                .find(|u| !u.is_whole())
                .map(|u| u.episode.max(0) as u32)
                .unwrap();
            let tag = intro_tag("blind-seeds", episode);
            (unit.row.clone(), (tag..tag + 200).collect())
        })
        .collect();
    let baseline = api::fingerprint_job::analyze_fingerprints_with_outro_engine(
        f.engine.as_ref(),
        f.media_id,
        1,
        known_intro,
        Vec::new(),
    )
    .unwrap();
    let replacement = run(&f);
    let intro_count = replacement
        .markers
        .iter()
        .filter(|marker| marker.intro_start_ms.is_some())
        .count();
    f.store
        .as_ref()
        .complete_marker_refresh("review9-job", &[replacement])
        .unwrap();
    let after = f
        .store
        .as_ref()
        .get_fixture_marker(f.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    println!(
        "baseline_intros={} adaptive_intros={intro_count} captures={} published_E1_intro={:?} job={}",
        baseline.len(),
        f.engine.reads.load(Ordering::Relaxed),
        after.intro_start_ms,
        f.store
            .as_ref()
            .get_probe_job("review9-job")
            .unwrap()
            .unwrap()
            .status
    );
    assert_eq!(
        baseline.len(),
        5,
        "fixture must provide five independent matching episodes"
    );
    assert_eq!(
        intro_count,
        baseline.len(),
        "full-window nonseed evidence must be analyzed before clearing old markers"
    );
}

#[test]
fn unlocked_timeline_uses_the_known_media_duration() {
    let f = setup("no-outro", &[1, 2, 3, 4]);
    let replacement = run(&f);
    f.store
        .as_ref()
        .complete_marker_refresh("review9-job", &[replacement])
        .unwrap();
    let chapters =
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(marker_timeline::resolved_chapters(
                f.store.clone(),
                f._tmp.path(),
                &f.units[0].row,
            ));
    let feature = chapters
        .iter()
        .find(|chapter| chapter.title.as_deref() == Some("正片"))
        .unwrap();
    println!(
        "known_duration_ms=1000000 feature_range=({}, {})",
        feature.start_ms, feature.end_ms
    );
    assert_eq!(
        feature.end_ms, 1_000_000,
        "resolved chapter timeline must end at known media duration"
    );
}

#[test]
fn missing_duration_does_not_publish_a_successful_outro_deletion() {
    let f = setup("uniform", &[1, 2, 3]);
    let first = &f.units[0].row;
    {
        let st = f.store.as_ref();
        let version = st
            .get_file_version(first.file.file_id)
            .unwrap()
            .expect("published row must carry a file version")
            .source_version;
        st.put_file_meta_versioned(
            &first.file.file_id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack::default()),
                audio: audio_tracks(),
                subtitles: vec![],
            },
            Some(&version),
            None,
        )
        .unwrap();
        let mut previous = st
            .get_fixture_marker(f.media_id, Some(1), Some(1))
            .unwrap()
            .unwrap();
        previous.outro_start_ms = Some(930_000);
        previous.outro_end_ms = Some(990_000);
        st.put_fixture_marker(&previous).unwrap();
    }
    assert!(
        try_run(&f).is_err(),
        "unsampled outro must reject forced refresh"
    );
    let after = f
        .store
        .as_ref()
        .get_fixture_marker(f.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    let status = f
        .store
        .as_ref()
        .get_probe_job("review9-job")
        .unwrap()
        .unwrap()
        .status;
    println!(
        "unknown_E1_duration: job={status} captures={} old_outro=Some(930000) new_outro={:?}",
        f.engine.reads.load(Ordering::Relaxed),
        after.outro_start_ms
    );
    assert_eq!(
        after.outro_start_ms,
        Some(930_000),
        "no outro sample was taken; missing duration cannot confirm absence of an outro"
    );
}

#[test]
fn another_common_variant_is_discovered_after_full_fallbacks() {
    let f = setup("other-variant", &[1, 2, 3, 4, 5, 6, 7, 8]);
    let known_intro: Vec<_> = f
        .units
        .iter()
        .map(|unit| {
            let episode = unit
                .row
                .units
                .iter()
                .find(|u| !u.is_whole())
                .map(|u| u.episode.max(0) as u32)
                .unwrap();
            let tag = intro_tag("other-variant", episode);
            (unit.row.clone(), (tag..tag + 200).collect())
        })
        .collect();
    let baseline = api::fingerprint_job::analyze_fingerprints_with_outro_engine(
        f.engine.as_ref(),
        f.media_id,
        1,
        known_intro,
        Vec::new(),
    )
    .unwrap();
    let replacement = run(&f);
    let intro_episodes: Vec<_> = replacement
        .markers
        .iter()
        .filter(|marker| marker.intro_start_ms.is_some())
        .map(|marker| marker.unit.episode)
        .collect();
    f.store
        .as_ref()
        .complete_marker_refresh("review9-job", &[replacement])
        .unwrap();
    let after = f
        .store
        .as_ref()
        .get_fixture_marker(f.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    println!(
        "two variants: baseline_intros={} adaptive_intros={intro_episodes:?} captures={} published_E1_intro={:?} job={}",
        baseline.len(),
        f.engine.reads.load(Ordering::Relaxed),
        after.intro_start_ms,
        f.store
            .as_ref()
            .get_probe_job("review9-job")
            .unwrap()
            .unwrap()
            .status
    );
    assert_eq!(
        baseline.len(),
        8,
        "both three-episode and five-episode variants are independently supported"
    );
    assert_eq!(
        intro_episodes.len(),
        baseline.len(),
        "nonseed fallback evidence must discover the second common variant"
    );
}

#[test]
fn new_variant_with_inconclusive_boundary_does_not_clear_old_intro() {
    let f = setup("edge-variant", &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(try_run(&f).is_err());
    let old = f
        .store
        .as_ref()
        .get_fixture_marker(f.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(old.intro_start_ms, Some(10_000));
    assert_eq!(old.source, "old");
}

#[test]
fn two_versions_of_one_episode_do_not_merge_distinct_intro_models() {
    let f = setup("mixed-versions", &[1, 2, 3, 1, 4, 5]);
    let replacement = run(&f);
    println!(
        "mixed versions markers={:?}",
        replacement
            .markers
            .iter()
            .map(|m| (m.unit.episode, m.intro_start_ms))
            .collect::<Vec<_>>()
    );
    let result = replacement
        .markers
        .iter()
        .find(|m| m.unit.episode == 4)
        .unwrap();
    assert_eq!(
        result.intro_start_ms,
        Some(10_000),
        "E4 matches E1's second version and E5, independently supporting its model"
    );
}

#[test]
fn missing_duration_preserves_start_only_outro_marker() {
    let f = setup("uniform", &[1, 2, 3]);
    let first = &f.units[0].row;
    {
        let st = f.store.as_ref();
        let version = st
            .get_file_version(first.file.file_id)
            .unwrap()
            .expect("published row must carry a file version")
            .source_version;
        st.put_file_meta_versioned(
            &first.file.file_id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack::default()),
                audio: audio_tracks(),
                subtitles: vec![],
            },
            Some(&version),
            None,
        )
        .unwrap();
        let mut previous = st
            .get_fixture_marker(f.media_id, Some(1), Some(1))
            .unwrap()
            .unwrap();
        previous.outro_start_ms = Some(930_000);
        previous.outro_end_ms = None;
        st.put_fixture_marker(&previous).unwrap();
    }
    assert!(
        try_run(&f).is_err(),
        "unsampled outro must reject forced refresh"
    );
    let after = f
        .store
        .as_ref()
        .get_fixture_marker(f.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    println!(
        "unknown duration + start-only outro={:?}/{:?}",
        after.outro_start_ms, after.outro_end_ms
    );
    assert_eq!(
        after.outro_start_ms,
        Some(930_000),
        "unsampled valid credits start must be preserved"
    );
}

#[test]
fn conflicting_version_boundaries_publish_each_files_own_range() {
    let f = setup("version-conflict", &[1, 2, 3, 1]);
    let replacement = run(&f);
    let chapter_updates = replacement.chapter_updates.clone();
    let members = store::observe_sources(
        &f.store
            .source_snapshots(
                &f.units
                    .iter()
                    .map(|unit| unit.row.file.file_id)
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
    );
    f.store
        .complete_marker_refresh_checked("review9-job", &[replacement], &members)
        .unwrap();
    for unit in f.units.iter().filter(|unit| unit.row.units[0].episode == 1) {
        let marker = f
            .store
            .get_file_marker(unit.row.file.file_id, unit.row.units[0])
            .unwrap()
            .unwrap();
        let version_three = unit
            .row
            .file
            .path
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("-3");
        let expected = if version_three {
            (Some(30_000), Some(90_000))
        } else {
            (Some(10_000), Some(70_000))
        };
        assert_eq!((marker.intro_start_ms, marker.intro_end_ms), expected);
        let chapters = &chapter_updates
            .iter()
            .find(|(id, _)| id == &unit.row.file.file_id.to_string())
            .unwrap()
            .1;
        let intro = chapters
            .iter()
            .find(|chapter| chapter.marker_type == Some(marker::MarkerType::IntroStart))
            .unwrap();
        assert_eq!((Some(intro.start_ms), Some(intro.end_ms)), expected);
    }
    assert_eq!(
        f.store
            .get_probe_job("review9-job")
            .unwrap()
            .unwrap()
            .status,
        "succeeded"
    );
}
