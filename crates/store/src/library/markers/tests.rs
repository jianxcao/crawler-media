use super::{MarkerResultReplacement, Store, StoredMediaMarker};
use crate::probe_tasks::ProbeJobUnitSpec;

fn marker(
    media_id: domain::MediaId,
    season: u32,
    source: &str,
    start_ms: i64,
) -> StoredMediaMarker {
    StoredMediaMarker {
        media_id,
        season,
        episode: 1,
        intro_start_ms: Some(start_ms),
        intro_end_ms: Some(start_ms + 60_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: source.into(),
        locked: false,
        updated_at: 1,
    }
}

/// Seed two seasons with old markers/chapters and finish both units of a refresh job.
fn two_season_refresh_setup(
    store: &Store,
    media_id: domain::MediaId,
    job_id: &str,
) -> Vec<library::ChapterMarker> {
    let old_chapters = vec![library::ChapterMarker {
        start_ms: 0,
        end_ms: 300_000,
        title: Some("原章节".into()),
        marker_type: None,
        synthetic: false,
    }];
    for season in [1, 2] {
        store
            .put_media_marker(&marker(media_id, season, "old", season as i64 * 100_000))
            .unwrap();
        store
            .put_cached_chapters(&format!("ledger-s{season}"), &old_chapters)
            .unwrap();
    }
    let specs = ["ledger-s1", "ledger-s2"].map(|ledger_id| ProbeJobUnitSpec {
        ledger_id,
        kind: "tv",
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: true,
    });
    assert!(
        store
            .create_probe_job(
                job_id,
                "marker_refresh",
                &media_id.to_string(),
                None,
                "marker-refresh-test",
                &specs,
            )
            .unwrap()
    );
    for ledger_id in ["ledger-s1", "ledger-s2"] {
        assert!(store.start_probe_unit(job_id, ledger_id).unwrap());
        store
            .finish_probe_unit(job_id, ledger_id, true, None)
            .unwrap();
    }
    old_chapters
}

#[test]
fn multi_season_refresh_keeps_old_results_on_failure_and_completes_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let media_id = domain::MediaId::new();
    let job_id = "multi-season-refresh";
    let old_chapters = two_season_refresh_setup(&store, media_id, job_id);

    let s1_new = marker(media_id, 1, "new", 111_000);
    let s2_duplicate = marker(media_id, 2, "new", 222_000);
    let replacements = [
        MarkerResultReplacement {
            media_id,
            season: 1,
            markers: vec![s1_new],
            chapter_updates: vec![(
                "ledger-s1".into(),
                vec![library::ChapterMarker {
                    start_ms: 111_000,
                    end_ms: 171_000,
                    title: Some("新片头".into()),
                    marker_type: Some(library::MarkerType::IntroStart),
                    synthetic: false,
                }],
            )],
        },
        MarkerResultReplacement {
            media_id,
            season: 2,
            markers: vec![s2_duplicate.clone(), s2_duplicate],
            chapter_updates: vec![("ledger-s2".into(), Vec::new())],
        },
    ];

    assert!(
        store
            .complete_marker_refresh(job_id, &replacements)
            .is_err()
    );
    assert_eq!(
        store.get_probe_job(job_id).unwrap().unwrap().status,
        "running"
    );
    for season in [1, 2] {
        assert_eq!(
            store
                .get_media_marker(media_id, Some(season), Some(1))
                .unwrap()
                .unwrap()
                .source,
            "old"
        );
        assert_eq!(
            store
                .get_cached_chapters(&format!("ledger-s{season}"))
                .unwrap()
                .unwrap(),
            old_chapters
        );
    }

    let valid_replacements = [
        MarkerResultReplacement {
            media_id,
            season: 1,
            markers: vec![marker(media_id, 1, "new", 111_000)],
            chapter_updates: vec![("ledger-s1".into(), old_chapters.clone())],
        },
        MarkerResultReplacement {
            media_id,
            season: 2,
            markers: vec![marker(media_id, 2, "new", 222_000)],
            chapter_updates: vec![("ledger-s2".into(), old_chapters.clone())],
        },
    ];
    store
        .complete_marker_refresh(job_id, &valid_replacements)
        .unwrap();
    assert_eq!(
        store.get_probe_job(job_id).unwrap().unwrap().status,
        "succeeded"
    );
    for (season, expected_source) in [(1, "new"), (2, "new")] {
        assert_eq!(
            store
                .get_media_marker(media_id, Some(season), Some(1))
                .unwrap()
                .unwrap()
                .source,
            expected_source
        );
    }
}

#[test]
fn locked_marker_preserves_and_syncs_chapters_across_ledger_versions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let media_id = domain::MediaId::new();
    let rows = seed_locked_versions(&store, media_id, dir.path());

    let intro_from_cache = |id: &str| -> Option<Vec<(i64, i64)>> {
        store.get_cached_chapters(id).unwrap().map(|c| {
            c.into_iter()
                .filter(|c| c.marker_type == Some(library::MarkerType::IntroStart))
                .map(|c| (c.start_ms, c.end_ms))
                .collect()
        })
    };

    // When detected replacement arrives with 40s..100s, locked marker is preserved
    // and all versions are synchronized to locked marker (10s..70s).
    let detected_marker = StoredMediaMarker {
        media_id,
        season: 1,
        episode: 1,
        intro_start_ms: Some(40_000),
        intro_end_ms: Some(100_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "voiceprint".into(),
        locked: false,
        updated_at: 2,
    };
    let updates: Vec<_> = rows
        .iter()
        .map(|r| {
            (
                r.id.to_string(),
                library::build_complete_timeline_chapters(&[], Some((40_000, 100_000)), None, None),
            )
        })
        .collect();
    store
        .replace_marker_results(media_id, 1, &[detected_marker], &updates)
        .unwrap();

    let cur_marker = store
        .get_media_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(cur_marker.intro_start_ms, Some(10_000));
    assert_eq!(cur_marker.intro_end_ms, Some(70_000));
    for r in &rows {
        assert_eq!(
            intro_from_cache(&r.id.to_string()),
            Some(vec![(10_000, 70_000)])
        );
    }

    // When empty replacements arrive, locked marker and chapters still remain 10s..70s.
    let empty_updates: Vec<_> = rows
        .iter()
        .map(|r| (r.id.to_string(), Vec::new()))
        .collect();
    store
        .replace_marker_results(media_id, 1, &[], &empty_updates)
        .unwrap();

    let cur_marker = store
        .get_media_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(cur_marker.intro_start_ms, Some(10_000));
    assert_eq!(cur_marker.intro_end_ms, Some(70_000));
    for r in &rows {
        assert_eq!(
            intro_from_cache(&r.id.to_string()),
            Some(vec![(10_000, 70_000)])
        );
    }
}

#[test]
fn locked_marker_rebuild_keeps_feature_within_known_media_duration() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let media_id = domain::MediaId::new();
    let ledger = domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id,
        path: format!("{}/feature.mkv", dir.path().display()),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&ledger).unwrap();
    let ledger_id = ledger.id.to_string();

    store
        .put_media_marker(&StoredMediaMarker {
            media_id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(10_000),
            intro_end_ms: Some(70_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "fixture".into(),
            locked: true,
            updated_at: 1,
        })
        .unwrap();
    store
        .put_file_meta_versioned(
            &ledger_id,
            &library::Tracks {
                video: None,
                audio: Vec::new(),
                subtitles: Vec::new(),
            },
            None,
            Some(1_000_000),
        )
        .unwrap();
    store
        .put_cached_chapters(
            &ledger_id,
            &library::build_complete_timeline_chapters(
                &[],
                Some((10_000, 70_000)),
                None,
                Some(1_000_000),
            ),
        )
        .unwrap();

    let feature_end = |store: &Store| -> Option<i64> {
        store
            .get_cached_chapters(&ledger_id)
            .unwrap()
            .and_then(|chapters| {
                chapters
                    .into_iter()
                    .find(|chapter| chapter.title.as_deref() == Some("正片"))
                    .map(|chapter| chapter.end_ms)
            })
    };
    assert_eq!(feature_end(&store), Some(1_000_000));

    store
        .replace_marker_results(media_id, 1, &[], &[(ledger_id.clone(), Vec::new())])
        .unwrap();

    assert_eq!(
        feature_end(&store),
        Some(1_000_000),
        "locked rebuild must keep the feature chapter inside the known media duration"
    );
}

#[test]
fn batch_replacement_stores_updated_at_in_unix_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let media_id = domain::MediaId::new();

    let mut detected = marker(media_id, 1, "voiceprint", 10_000);
    detected.updated_at = 0;
    let before_write = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    store
        .replace_marker_results(media_id, 1, &[detected], &[])
        .unwrap();
    let after_write = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let stored = store
        .get_media_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert!(
        (before_write..=after_write).contains(&stored.updated_at),
        "batch replacement must write the current Unix timestamp in seconds"
    );
}

fn seed_locked_versions(
    store: &Store,
    media_id: domain::MediaId,
    root: &std::path::Path,
) -> Vec<domain::LedgerRow> {
    let rows: Vec<_> = (0..3)
        .map(|v| domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id,
            path: format!("{}/version-{v}.mkv", root.display()),
            season: Some(1),
            episode: Some(1),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High,
            filter_score: None,
        })
        .collect();
    for r in &rows {
        store.insert_ledger(r).unwrap();
    }
    let locked_marker = StoredMediaMarker {
        media_id,
        season: 1,
        episode: 1,
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "fixture".into(),
        locked: true,
        updated_at: 1,
    };
    store.put_media_marker(&locked_marker).unwrap();
    store
        .put_cached_chapters(
            &rows[0].id.to_string(),
            &library::build_complete_timeline_chapters(&[], Some((10_000, 70_000)), None, None),
        )
        .unwrap();
    store
        .put_cached_chapters(
            &rows[1].id.to_string(),
            &library::build_complete_timeline_chapters(&[], Some((20_000, 80_000)), None, None),
        )
        .unwrap();
    store
        .put_cached_chapters(&rows[2].id.to_string(), &[])
        .unwrap();

    rows
}
