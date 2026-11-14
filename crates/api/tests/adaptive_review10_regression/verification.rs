use super::*;

#[test]
fn adding_versions_of_one_reference_must_not_change_independent_consensus() {
    let control = setup("vote-duplication", &[1, 2, 3, 4]);
    let correct = run(&control)
        .markers
        .into_iter()
        .find(|m| m.unit.episode == 4)
        .unwrap();
    assert_eq!(
        (correct.intro_start_ms, correct.intro_end_ms),
        (Some(100_000), Some(160_000))
    );
    let f = setup("vote-duplication", &[1, 2, 3, 4, 1, 1]);
    let replacement = run(&f);
    let target = replacement
        .markers
        .iter()
        .find(|m| m.unit.episode == 4)
        .unwrap();
    println!(
        "E4 control={:?}..{:?}; three versions of E1={:?}..{:?}",
        correct.intro_start_ms, correct.intro_end_ms, target.intro_start_ms, target.intro_end_ms
    );
    assert_eq!(
        (target.intro_start_ms, target.intro_end_ms),
        (correct.intro_start_ms, correct.intro_end_ms),
        "repeating one episode must not outvote two independent reference episodes"
    );
}

#[test]
fn ambiguous_final_verification_does_not_publish_an_earlier_provisional_match() {
    let f = setup("ambiguous-final", &[1, 2, 3, 4, 5]);
    assert!(try_run(&f).is_err());
    let outcome = final_intro_outcome(&f, 4);
    assert!(
        matches!(
            outcome,
            marker::adaptive::VerificationOutcome::NeedsFullWindow { .. }
        ),
        "{outcome:?}"
    );
    let old = f
        .store
        .as_ref()
        .get_fixture_marker(f.media_id, Some(1), Some(4))
        .unwrap()
        .unwrap();
    assert_eq!(
        (old.intro_start_ms, old.intro_end_ms),
        (Some(10_000), Some(70_000))
    );
    assert_eq!(old.source, "old");
}

#[test]
fn explicit_no_match_in_one_file_does_not_clear_another_cuts_intro() {
    let f = setup("version-missing", &[1, 2, 3, 1]);
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
            (None, None)
        } else {
            (Some(10_000), Some(70_000))
        };
        assert_eq!((marker.intro_start_ms, marker.intro_end_ms), expected);
        let chapters = &chapter_updates
            .iter()
            .find(|(id, _)| id == &unit.row.file.file_id.to_string())
            .unwrap()
            .1;
        assert_eq!(
            chapters
                .iter()
                .any(|chapter| chapter.marker_type == Some(marker::MarkerType::IntroStart)),
            !version_three
        );
    }
}

#[test]
fn unresolved_final_verification_cannot_mark_forced_refresh_successful() {
    let f = setup("edge-variant", &[1, 2, 3, 4, 5, 6, 7, 8]);
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(api::probe_manager::season::run_adaptive_season_pipeline(
            &f.manager,
            "review9-job",
            f.media_id,
            1,
            &f.units,
        ));
    let rejected = result.is_err();
    if let Ok(replacement) = result {
        f.store
            .as_ref()
            .complete_marker_refresh("review9-job", &[replacement])
            .unwrap();
        let marker = f
            .store
            .as_ref()
            .get_fixture_marker(f.media_id, Some(1), Some(1))
            .unwrap()
            .unwrap();
        println!(
            "unresolved E1 job={} source={} intro={:?}..{:?}",
            f.store
                .as_ref()
                .get_probe_job("review9-job")
                .unwrap()
                .unwrap()
                .status,
            marker.source,
            marker.intro_start_ms,
            marker.intro_end_ms
        );
    }
    assert!(
        rejected,
        "a forced refresh with a final NeedsFullWindow must fail and retain old result instead of relabeling it as freshly generated success"
    );
}
