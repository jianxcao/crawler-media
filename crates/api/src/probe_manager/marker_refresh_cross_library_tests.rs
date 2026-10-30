// Season split across two libraries, refresh requested from library A only.
#[test]
fn window6_cross_library_season_refresh_publishes_only_job_members() {
    let harness = RefreshHarness::new();
    let lib_b_dir = harness._tmp.path().join("tv_lib_b");
    std::fs::create_dir_all(&lib_b_dir).unwrap();
    {
        let store = harness.store.as_ref();
        let library = store
            .create_library(
                MediaKind::Tv,
                "TV B",
                &[lib_b_dir.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store
            .set_library_intro_settings(&library.id, true, true)
            .unwrap();
    }
    let mut row_b = super::super::test_support::coverage(
        harness.media_id,
        &lib_b_dir
            .join("Pantheon.S01E03.mkv")
            .to_string_lossy()
            .into_owned(),
        Some(1),
        Some(3),
    );
    std::fs::write(&row_b.file.path, b"dummy").unwrap();
    let expected_chapters_b = {
        let store = harness.store.as_ref();
        super::super::test_support::publish_into(&store, &mut row_b);
        seed_episode_old_refresh_result(
            &store,
            harness.media_id,
            &row_b.file.file_id.to_string(),
            3,
        )
    };

    let units: Vec<ProbeUnit> = harness
        .rows
        .iter()
        .map(|row| ProbeUnit {
            row: row.clone(),
            kind: MediaKind::Tv,
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: true,
            marker_refresh_id: None,
            job_id: None,
        })
        .collect();
    assert_eq!(harness.manager.enqueue_marker_refresh(units), 2);
    let job_id = {
        let store = harness.store.as_ref();
        store
            .active_probe_unit_for_ledger(&harness.rows[0].file.file_id.to_string())
            .unwrap()
            .unwrap()
            .job_id
    };

    let first = harness.manager.take_queued_for_test().unwrap();
    let second = harness.manager.take_queued_for_test().unwrap();
    harness.manager.finish(&first, true);
    harness.manager.finish(&second, true);

    let (job, chapters_b, chapters_a0, chapters_a1, marker_a0, marker_a1, marker_b, active_job) = {
        let store = harness.store.as_ref();
        (
            store.get_probe_job(&job_id).unwrap().unwrap(),
            store
                .get_cached_chapters(&row_b.file.file_id.to_string())
                .unwrap(),
            store
                .get_cached_chapters(&harness.rows[0].file.file_id.to_string())
                .unwrap(),
            store
                .get_cached_chapters(&harness.rows[1].file.file_id.to_string())
                .unwrap(),
            store
                .get_media_marker(harness.media_id, Some(1), Some(1))
                .unwrap(),
            store
                .get_media_marker(harness.media_id, Some(1), Some(2))
                .unwrap(),
            store
                .get_media_marker(
                    harness.media_id,
                    super::super::single_season_opt(&row_b),
                    super::super::single_episode_opt(&row_b),
                )
                .unwrap(),
            store.active_probe_job_for_scope(&harness.scope).unwrap(),
        )
    };
    assert_eq!(job.status, "succeeded");
    assert_eq!(chapters_b, Some(expected_chapters_b));
    assert_eq!(chapters_a0.as_ref(), Some(&harness.expected_chapters[0]));
    assert_eq!(chapters_a1.as_ref(), Some(&harness.expected_chapters[1]));
    assert!(marker_a0.is_none());
    assert!(marker_a1.is_none());
    let marker_b = marker_b.expect("library B marker must still exist");
    assert_eq!(marker_b.source, "old-result");
    assert_eq!(marker_b.intro_start_ms, Some(111_000));
    assert_eq!(marker_b.intro_end_ms, Some(222_000));
    assert_eq!(marker_b.outro_start_ms, Some(1_000_000));
    assert_eq!(marker_b.outro_end_ms, Some(1_050_000));
    assert!(active_job.is_none());
}
