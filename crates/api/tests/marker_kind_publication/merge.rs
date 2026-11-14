use super::*;
pub(super) fn old_marker(s: &ProbeScenario, locked: bool) -> store::StoredFileMarker {
    let row = &s.ledgers[0];
    let marker = store::StoredFileMarker {
        file_id: row.file.file_id,
        version: row.file.version.clone(),
        unit: row.units[0],
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: Some(2_300_000),
        outro_end_ms: Some(2_350_000),
        source: if locked { "manual" } else { "fingerprint" }.into(),
        locked,
        updated_at: 1,
    };
    s.store.put_fixture_marker(&marker).unwrap();
    s.store
        .get_file_marker(marker.file_id, marker.unit)
        .unwrap()
        .unwrap()
}
pub(super) fn marker(s: &ProbeScenario) -> store::StoredFileMarker {
    let row = &s.ledgers[0];
    s.store
        .get_file_marker(row.file.file_id, row.units[0])
        .unwrap()
        .unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_outro_retains_only_accepted_current_version_value() {
    let s = scenario("full_window").await;
    let old = old_marker(&s, false);
    let job = run(&s, false).await;
    let after = marker(&s);
    assert_eq!(after.intro_start_ms, Some(30_000));
    assert_eq!(
        (after.outro_start_ms, after.outro_end_ms),
        (old.outro_start_ms, old.outro_end_ms)
    );
    let outcome = outcome(&s, &job);
    assert_eq!(outcome["status"], "partial");
    assert_eq!(outcome["kinds"]["outro"]["retained_previous"], true);
    assert_eq!(outcome["kinds"]["intro"]["retained_previous"], false);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_new_revision_does_not_copy_previous_version_outro() {
    let mut s = scenario("full_window").await;
    old_marker(&s, false);
    let old = s.ledgers[0].clone();
    let id = old.file.file_id.to_string();
    let mut cache = s.store.get_fingerprint_cache(&id).unwrap().unwrap();
    let tracks = s.store.get_file_meta(&id).unwrap().unwrap();
    let mut file = old.file.clone();
    std::fs::write(&file.path, "http://127.0.0.1:8080/replaced.mkv\n").unwrap();
    file.version.source_version = api::fingerprint_job::current_source_version(&file.path);
    s.store
        .publish_files(
            &store::library_write::WriteScope {
                library_id: file.library_id,
                config_revision: s
                    .store
                    .library_config_revision(&file.library_id.to_string())
                    .unwrap(),
            },
            &[store::library_write::FilePublication {
                file: &file,
                units: &old.units,
                expected_version: Some(&old.file.version),
                nfo_observation: None,
                probe_required: false,
            }],
            2000,
        )
        .unwrap();
    s.ledgers[0] = s.store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    assert_ne!(s.ledgers[0].file.version, old.file.version);
    cache.cache_key = api::fingerprint_job::fingerprint_cache_key_with_profile(
        &s.ledgers[0].file.version.source_version,
        180,
        Some(2_400_000),
        &api::fingerprint_job::profile_for_source(Some(&support::fixture_source())),
    );
    s.store.put_fingerprint_cache(&id, &cache).unwrap();
    s.store
        .put_file_meta_versioned(
            &id,
            &tracks,
            Some(&s.ledgers[0].file.version.source_version),
            Some(2_400_000),
        )
        .unwrap();
    run(&s, false).await;
    let after = marker(&s);
    assert_eq!(after.version, s.ledgers[0].file.version);
    assert_eq!(after.intro_start_ms, Some(30_000));
    assert_eq!(after.outro_start_ms, None);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_uncertain_preserves_marker_chapters_and_frame_generation() {
    let mut s = scenario("full_window").await;
    run(&s, false).await;
    let before = marker(&s);
    let row = &s.ledgers[0];
    let chapters = s
        .store
        .get_cached_chapters(&row.file.file_id.to_string())
        .unwrap();
    let frames = s
        .store
        .current_chapter_frame_task(row.file.file_id)
        .unwrap();
    restart(
        &mut s,
        Arc::new(KindEngine {
            intro_secs: 3.0,
            outro_secs: 3.0,
        }),
    );
    let job = run(&s, true).await;
    assert_eq!(job.status, "succeeded");
    assert_eq!(marker(&s), before);
    let row = &s.ledgers[0];
    assert_eq!(
        s.store
            .get_cached_chapters(&row.file.file_id.to_string())
            .unwrap(),
        chapters
    );
    assert_eq!(
        s.store
            .current_chapter_frame_task(row.file.file_id)
            .unwrap(),
        frames
    );
    let outcome = outcome(&s, &job);
    assert_eq!(outcome["status"], "uncertain");
    for kind in ["intro", "outro"] {
        assert_eq!(outcome["kinds"][kind]["status"], "uncertain");
        assert!(
            outcome["kinds"][kind]["reason"]
                .as_str()
                .unwrap()
                .contains("verification_unresolved")
        );
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn locked_manual_marker_is_unchanged_by_partial_refresh() {
    let s = scenario("full_window").await;
    let old = old_marker(&s, true);
    let job = run(&s, true).await;
    assert_eq!(job.status, "succeeded");
    assert_eq!(marker(&s), old);
    let result = outcome(&s, &job);
    assert_eq!(result["kinds"]["intro"]["decision"], "verified");
    assert_eq!(
        result["kinds"]["intro"]["retained_previous"], true,
        "manual lock preserves the actual published kind despite fresh recognition"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strict_no_match_clears_only_covered_outro() {
    for covered in [true, false] {
        let mut s = scenario("full_window").await;
        s.manager.shutdown_workers().await.unwrap();
        let mut old = old_marker(&s, false);
        if !covered {
            old.outro_start_ms = Some(2_100_000);
            old.outro_end_ms = Some(2_150_000);
            s.store.put_fixture_marker(&old).unwrap();
        }
        restart(
            &mut s,
            Arc::new(KindEngine {
                intro_secs: 60.0,
                outro_secs: 0.0,
            }),
        );
        let job = run(&s, true).await;
        let after = marker(&s);
        assert_eq!(after.intro_start_ms, Some(30_000));
        assert_eq!(
            after.outro_start_ms,
            if covered { None } else { old.outro_start_ms }
        );
        let outcome = outcome(&s, &job);
        assert_eq!(
            outcome["status"],
            if covered { "verified" } else { "partial" }
        );
        assert_eq!(
            outcome["kinds"]["outro"]["decision"], "no_match",
            "raw NoMatch must remain visible even if absence coverage refuses clearing"
        );
        assert_eq!(
            outcome["kinds"]["outro"]["status"],
            if covered { "no_match" } else { "uncertain" }
        );
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn later_verified_outro_keeps_uncertain_current_intro() {
    let mut s = scenario("full_window").await;
    run(&s, false).await;
    restart(
        &mut s,
        Arc::new(KindEngine {
            intro_secs: 3.0,
            outro_secs: 60.0,
        }),
    );
    let job = run(&s, true).await;
    let after = marker(&s);
    assert_eq!(
        (after.intro_start_ms, after.intro_end_ms),
        (Some(30_000), Some(90_000))
    );
    assert_eq!(
        (after.outro_start_ms, after.outro_end_ms),
        (Some(2_250_000), Some(2_310_000))
    );
    assert_eq!(outcome(&s, &job)["status"], "partial");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_start_only_outro_survives_independent_intro_absence() {
    let mut s = scenario("full_window").await;
    s.manager.shutdown_workers().await.unwrap();
    let mut old = old_marker(&s, false);
    old.outro_end_ms = None;
    s.store.put_fixture_marker(&old).unwrap();
    restart(
        &mut s,
        Arc::new(KindEngine {
            intro_secs: 0.0,
            outro_secs: 3.0,
        }),
    );
    let job = run(&s, true).await;
    let after = marker(&s);
    assert_eq!((after.intro_start_ms, after.intro_end_ms), (None, None));
    assert_eq!(
        (after.outro_start_ms, after.outro_end_ms),
        (old.outro_start_ms, None)
    );
    let outcome = outcome(&s, &job);
    assert_eq!(outcome["status"], "partial");
    assert_eq!(outcome["kinds"]["outro"]["retained_previous"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn locked_empty_intro_is_reported_protected_from_fresh_recognition() {
    let s = scenario("full_window").await;
    let mut old = old_marker(&s, true);
    old.intro_start_ms = None;
    old.intro_end_ms = None;
    s.store.put_fixture_marker(&old).unwrap();
    let job = run(&s, true).await;
    let after = marker(&s);
    assert_eq!((after.intro_start_ms, after.intro_end_ms), (None, None));
    let result = outcome(&s, &job);
    assert_eq!(result["kinds"]["intro"]["decision"], "verified");
    assert_eq!(
        result["kinds"]["intro"]["retained_previous"], true,
        "protection retains the old absence too; the new Intro was not adopted"
    );
}
