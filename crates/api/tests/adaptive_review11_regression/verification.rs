use super::*;

#[test]
fn adding_versions_of_one_reference_must_not_change_independent_consensus() {
    let control = setup("vote-duplication", &[1, 2, 3, 4]);
    let correct = run(&control)
        .markers
        .into_iter()
        .find(|m| m.episode == 4)
        .unwrap();
    assert_eq!(
        (correct.intro_start_ms, correct.intro_end_ms),
        (Some(100_000), Some(160_000))
    );
    let f = setup("vote-duplication", &[1, 2, 3, 4, 1, 1]);
    let replacement = run(&f);
    let target = replacement.markers.iter().find(|m| m.episode == 4).unwrap();
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
    assert!(matches!(
        outcome,
        marker::adaptive::VerificationOutcome::NeedsFullWindow { .. }
    ));
    let old = f
        .store
        .lock()
        .get_media_marker(f.media_id, Some(1), Some(4))
        .unwrap()
        .unwrap();
    assert_eq!(
        (old.intro_start_ms, old.intro_end_ms),
        (Some(10_000), Some(70_000))
    );
    assert_eq!(old.source, "old");
}

#[test]
fn explicit_no_match_in_one_version_conflicts_with_detected_intro_in_another() {
    let f = setup("version-missing", &[1, 2, 3, 1]);
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
    let conflict = result
        .as_ref()
        .err()
        .map(|e| e.contains("episode_version_conflict"))
        .unwrap_or(false);
    if let Ok(replacement) = result {
        let ledger_b = &f.units[3].row;
        let has_intro = replacement
            .chapter_updates
            .iter()
            .find(|(id, _)| id == &ledger_b.id.to_string())
            .unwrap()
            .1
            .iter()
            .any(|c| c.marker_type == Some(marker::MarkerType::IntroStart));
        let canonical = replacement.markers.iter().find(|m| m.episode == 1).unwrap();
        println!(
            "E1 version B intro chapter={has_intro}; shared E1 intro={:?}..{:?}; B verdict={:?}",
            canonical.intro_start_ms,
            canonical.intro_end_ms,
            final_intro_outcome(&f, 1)
        );
    }
    assert!(
        conflict,
        "one version explicitly has no intro while the other has one; a shared episode marker cannot represent both"
    );
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
            .lock()
            .complete_marker_refresh("review9-job", &[replacement])
            .unwrap();
        let marker = f
            .store
            .lock()
            .get_media_marker(f.media_id, Some(1), Some(1))
            .unwrap()
            .unwrap();
        println!(
            "unresolved E1 job={} source={} intro={:?}..{:?}",
            f.store
                .lock()
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
