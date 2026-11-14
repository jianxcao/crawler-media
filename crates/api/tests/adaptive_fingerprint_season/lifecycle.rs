use super::*;
use api::{
    ScrapeStoreExt,
    probe_manager::clock::{FakeClock, ProbeClock},
};

pub(super) fn setup() -> (
    tempfile::TempDir,
    Arc<Store>,
    Arc<ProbeManager>,
    Arc<FakeClock>,
    MediaId,
    std::path::PathBuf,
) {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(temp.path()).unwrap());
    let media = domain::Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Convergence".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let root = temp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    store::test_support::reconfigure_default_root(&store, MediaKind::Tv, &root).unwrap();
    let library = store.default_library(MediaKind::Tv).unwrap().unwrap();
    store
        .set_library_intro_settings(&library.id, true, true)
        .unwrap();
    let mut config = store.get_scrape_config().unwrap().setting;
    config.fingerprint_sampling_mode = Some("adaptive".into());
    store.save_scrape_config(&config).unwrap();
    let clock = Arc::new(FakeClock::new(0));
    let manager = Arc::new(
        ProbeManager::with_engines(
            store.clone(),
            Arc::new(SharedMatchEngine),
            Arc::new(SharedMatchEngine),
        )
        .unwrap(),
    );
    manager.set_clock(clock.clone());
    manager.try_start_workers();
    (temp, store, manager, clock, media.id, root)
}

pub(super) async fn drain(
    store: &Store,
    manager: &ProbeManager,
    clock: &FakeClock,
    rows: &[FileCoverage],
) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            clock.set(clock.now_ms() + 2_000);
            manager.wake_dispatch();
            if rows.iter().all(|row| {
                store
                    .active_probe_unit_for_ledger(&row.file.file_id.to_string())
                    .unwrap()
                    .is_none()
            }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "selected files and their successors must drain: {error}: {:?}",
            store.recover_probe_jobs().unwrap()
        )
    });
}

#[path = "fix1.rs"]
mod fix1;
#[path = "recapture.rs"]
mod recapture;
#[path = "themes.rs"]
mod themes;

#[tokio::test]
async fn unknown_duration_skips_outro_and_short_duration_bounds_both_windows() {
    for duration in [None, Some(60_000)] {
        let (_temp, store, manager, clock, media, root) = setup();
        let row = row(&store, media, &root, 1, 1);
        let tracks = store
            .get_file_meta(&row.file.file_id.to_string())
            .unwrap()
            .unwrap();
        store
            .put_file_meta_versioned(
                &row.file.file_id.to_string(),
                &tracks,
                Some(&row.file.version.source_version),
                duration,
            )
            .unwrap();
        let job = manager
            .enqueue_direct_fingerprint(
                unit(row.clone()),
                duration,
                row.file.version.source_version.clone(),
                180,
            )
            .unwrap();
        drain(&store, &manager, &clock, std::slice::from_ref(&row)).await;
        let attempts = store.list_fingerprint_attempts_for_job(&job).unwrap();
        assert_eq!(attempts.len(), if duration.is_some() { 2 } else { 1 });
        for attempt in attempts {
            assert_eq!(
                (attempt.window_start_ms, attempt.window_end_ms),
                (0, duration.unwrap_or(180_000))
            );
            if duration.is_none() {
                assert_eq!(attempt.kind, "intro");
            }
        }
        manager.shutdown_workers().await.unwrap();
    }
}

#[tokio::test]
async fn stable_outro_uses_current_metadata_duration_without_spurious_fallback() {
    let (_temp, store, manager, clock, media, root) = setup();
    let mut rows = Vec::new();
    for episode in 1..=4 {
        let row = row(&store, media, &root, 1, episode);
        let job = manager
            .enqueue_direct_fingerprint(
                unit(row.clone()),
                Some(1_800_000),
                row.file.version.source_version.clone(),
                180,
            )
            .unwrap();
        rows.push(row.clone());
        drain(&store, &manager, &clock, &rows).await;
        if episode == 4 {
            let attempts = store.list_fingerprint_attempts_for_job(&job).unwrap();
            assert_eq!(
                attempts.len(),
                2,
                "one current template window per kind: {attempts:?}"
            );
            let tail = attempts
                .iter()
                .find(|attempt| attempt.kind == "outro")
                .unwrap();
            assert_eq!(
                (tail.window_start_ms, tail.window_end_ms),
                (1_620_000, 1_720_000)
            );
            let marker = store
                .get_file_marker(row.file.file_id, row.units[0])
                .unwrap()
                .unwrap();
            assert_eq!(
                (marker.outro_start_ms, marker.outro_end_ms),
                (Some(1_630_000), Some(1_710_000))
            );
        }
    }
    manager.shutdown_workers().await.unwrap();
}

fn row(
    store: &Store,
    media: MediaId,
    root: &std::path::Path,
    season: i32,
    episode: u32,
) -> FileCoverage {
    let path = root.join(format!("S{season}E{episode}.mkv"));
    std::fs::write(&path, b"fixture").unwrap();
    let fixture = publish_coverage(store, media, &path, episode);
    if season != 1 {
        store
            .publish_files(
                &store::library_write::WriteScope {
                    library_id: fixture.file.library_id,
                    config_revision: store
                        .library_config_revision(&fixture.file.library_id.to_string())
                        .unwrap(),
                },
                &[store::library_write::FilePublication {
                    file: &fixture.file,
                    units: &[UnitKey::new(season, episode as i32)],
                    expected_version: Some(&fixture.file.version),
                    nfo_observation: None,
                    probe_required: false,
                }],
                1000,
            )
            .unwrap();
    }
    store
        .file_coverage_by_id(fixture.file.file_id)
        .unwrap()
        .unwrap()
}

pub(super) fn unit(row: FileCoverage) -> ProbeUnit {
    ProbeUnit {
        row,
        kind: MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: true,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    }
}

#[tokio::test]
async fn queued_participants_reconcile_their_own_previous_reference_before_joint_claim() {
    let (_temp, store, manager, clock, media, root) = setup();
    let mut rows = Vec::new();
    for episode in 1..=3 {
        let row = row(&store, media, &root, 1, episode);
        manager
            .enqueue_direct_fingerprint(
                unit(row.clone()),
                Some(1_800_000),
                row.file.version.source_version.clone(),
                180,
            )
            .unwrap();
        rows.push(row);
    }
    let participants = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let participants = store
                .probe_analysis_participants(media, 1, None, 16)
                .unwrap();
            if participants.len() == 3 {
                break participants;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        store
            .refresh_probe_analysis_targets(&store::ProbeAnalysisReconcile {
                trigger: participants[0].clone(),
                expected_cursor: None,
                reference_key: "previous-reference".into(),
                candidates: participants[1..].to_vec(),
                next_cursor: None,
            })
            .unwrap()
    );
    drain(&store, &manager, &clock, &rows).await;
    for participant in participants {
        let units = store
            .probe_job_units(&participant.dispatch().job_id)
            .unwrap();
        let outcome: serde_json::Value =
            serde_json::from_str(units[0].detection_outcome_json.as_deref().unwrap()).unwrap();
        assert_eq!(units[0].status, "succeeded");
        assert_eq!(outcome["status"], "verified");
        assert_ne!(outcome["available_reference_key"], "previous-reference");
        assert!(
            store
                .get_file_marker(
                    participant.context().target.file.file_id,
                    participant.context().target.units[0]
                )
                .unwrap()
                .is_some()
        );
    }
    manager.shutdown_workers().await.unwrap();
}

#[tokio::test]
async fn three_ordinary_arrivals_publish_complete_verified_results_without_refresh() {
    let (_temp, store, manager, clock, media, root) = setup();
    let mut rows = Vec::new();
    for episode in 1..=3 {
        let row = row(&store, media, &root, 1, episode);
        manager
            .enqueue_direct_fingerprint(
                unit(row.clone()),
                Some(1_800_000),
                row.file.version.source_version.clone(),
                180,
            )
            .unwrap();
        rows.push(row);
    }
    drain(&store, &manager, &clock, &rows).await;
    for row in &rows {
        verified_intervals(&store, row);
    }
    manager.shutdown_workers().await.unwrap();
}

#[tokio::test]
async fn twenty_four_batch_and_sequential_arrivals_publish_equal_intervals() {
    let mut results = Vec::new();
    for sequential in [false, true] {
        let (_temp, store, manager, clock, media, root) = setup();
        let mut rows = Vec::new();
        for episode in 1..=24 {
            let row = row(&store, media, &root, 1, episode);
            manager
                .enqueue_direct_fingerprint(
                    unit(row.clone()),
                    Some(1_800_000),
                    row.file.version.source_version.clone(),
                    180,
                )
                .unwrap();
            rows.push(row);
            if sequential {
                drain(&store, &manager, &clock, &rows).await;
            }
        }
        drain(&store, &manager, &clock, &rows).await;
        results.push(
            rows.iter()
                .map(|row| verified_intervals(&store, row))
                .collect::<Vec<_>>(),
        );
        manager.shutdown_workers().await.unwrap();
    }
    assert_eq!(results[0], results[1]);
}

fn verified_intervals(
    store: &Store,
    row: &FileCoverage,
) -> (Option<i64>, Option<i64>, Option<i64>, Option<i64>) {
    let marker = store
        .get_file_marker(row.file.file_id, row.units[0])
        .unwrap()
        .expect("independently supported episode");
    let intervals = (
        marker.intro_start_ms,
        marker.intro_end_ms,
        marker.outro_start_ms,
        marker.outro_end_ms,
    );
    assert_eq!(
        intervals,
        (Some(10_000), Some(90_000), Some(1_630_000), Some(1_710_000))
    );
    let job = store
        .latest_probe_job_for_ledger(&row.file.file_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(
        store.get_probe_job(&job).unwrap().unwrap().status,
        "succeeded"
    );
    let units = store.probe_job_units(&job).unwrap();
    let unit = units
        .iter()
        .find(|unit| unit.ledger_id == row.file.file_id.to_string())
        .unwrap();
    let outcome: serde_json::Value =
        serde_json::from_str(unit.detection_outcome_json.as_deref().unwrap()).unwrap();
    assert_eq!(unit.status, "succeeded");
    assert_eq!(outcome["status"], "verified");
    intervals
}

#[tokio::test]
async fn unresolved_second_season_preserves_itself_while_first_season_publishes() {
    let (_temp, store, manager, clock, media, root) = setup();
    let mut rows = (1..=3)
        .map(|episode| row(&store, media, &root, 1, episode))
        .collect::<Vec<_>>();
    rows.push(row(&store, media, &root, 2, 1));
    let mut old = Vec::new();
    let mut chapters = Vec::new();
    for row in &rows {
        store
            .put_fixture_marker(&store::StoredFileMarker {
                file_id: row.file.file_id,
                version: row.file.version.clone(),
                unit: row.units[0],
                intro_start_ms: Some(3_000),
                intro_end_ms: Some(8_000),
                outro_start_ms: None,
                outro_end_ms: None,
                source: "fingerprint".into(),
                locked: false,
                updated_at: 12,
            })
            .unwrap();
        old.push(
            store
                .get_file_marker(row.file.file_id, row.units[0])
                .unwrap(),
        );
        chapters.push(
            store
                .get_cached_chapters(&row.file.file_id.to_string())
                .unwrap(),
        );
    }
    let files = rows
        .iter()
        .map(|row| row.file.file_id.to_string())
        .collect::<Vec<_>>();
    let specs = files
        .iter()
        .map(|file| store::ProbeJobUnitSpec {
            ledger_id: file,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: true,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        })
        .collect::<Vec<_>>();
    assert!(
        store
            .create_probe_job(
                "all-seasons",
                "marker_refresh",
                &media.to_string(),
                None,
                "all-seasons",
                &specs
            )
            .unwrap()
    );
    manager.wake_dispatch();
    let job = store
        .active_probe_unit_for_ledger(&rows[0].file.file_id.to_string())
        .unwrap()
        .unwrap()
        .job_id;
    drain(&store, &manager, &clock, &rows).await;
    assert_eq!(
        store.get_probe_job(&job).unwrap().unwrap().status,
        "succeeded"
    );
    for (index, row) in rows.iter().enumerate() {
        let after = store
            .get_file_marker(row.file.file_id, row.units[0])
            .unwrap();
        let units = store.probe_job_units(&job).unwrap();
        let unit = units
            .iter()
            .find(|u| u.ledger_id == row.file.file_id.to_string())
            .unwrap();
        let outcome: serde_json::Value =
            serde_json::from_str(unit.detection_outcome_json.as_deref().unwrap()).unwrap();
        if row.units[0].season == 2 {
            assert_eq!(after, old[index]);
            assert_eq!(
                store
                    .get_cached_chapters(&row.file.file_id.to_string())
                    .unwrap(),
                chapters[index]
            );
            assert_eq!(outcome["status"], "uncertain");
        } else {
            assert_eq!(outcome["status"], "verified");
            let marker = after.unwrap();
            assert_eq!(
                (marker.intro_start_ms, marker.intro_end_ms),
                (Some(10_000), Some(90_000))
            );
            assert!(
                store
                    .current_chapter_frame_task(row.file.file_id)
                    .unwrap()
                    .is_some()
            );
        }
    }
    manager.shutdown_workers().await.unwrap();
}

#[tokio::test]
async fn second_season_sql_failure_rolls_back_every_output_and_recovers() {
    let (temp, store, manager, clock, media, root) = setup();
    let mut rows = Vec::new();
    for season in 1..=2 {
        for episode in 1..=3 {
            rows.push(row(&store, media, &root, season, episode));
        }
    }
    let ids = rows
        .iter()
        .map(|row| row.file.file_id.to_string())
        .collect::<Vec<_>>();
    let specs = ids
        .iter()
        .map(|id| store::ProbeJobUnitSpec {
            ledger_id: id,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: true,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        })
        .collect::<Vec<_>>();
    let db = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_second_season BEFORE INSERT ON file_markers WHEN NEW.season=2
        BEGIN SELECT RAISE(ABORT,'injected second season failure'); END",
    )
    .unwrap();
    assert!(
        store
            .create_probe_job(
                "atomic-seasons",
                "marker_refresh",
                &media.to_string(),
                None,
                "atomic-seasons",
                &specs
            )
            .unwrap()
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            clock.set(clock.now_ms() + 2_000);
            manager.wake_dispatch();
            if store
                .probe_job_units("atomic-seasons")
                .unwrap()
                .iter()
                .all(|unit| unit.stage == store::ProbeDispatchStage::Analysis)
                && manager
                    .timing_snapshot("atomic-seasons")
                    .is_some_and(|timing| timing.raw_pairs_computed.is_some_and(|n| n > 0))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        loop {
            if store
                .probe_job_units("atomic-seasons")
                .unwrap()
                .iter()
                .all(|unit| unit.status == "queued")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        store
            .get_probe_job("atomic-seasons")
            .unwrap()
            .unwrap()
            .is_active()
    );
    assert!(
        store
            .list_fingerprint_models(&media.to_string(), 1)
            .unwrap()
            .is_empty()
    );
    for row in &rows {
        assert!(
            store
                .get_file_marker(row.file.file_id, row.units[0])
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_cached_chapters(&row.file.file_id.to_string())
                .unwrap()
                .unwrap_or_default()
                .is_empty()
        );
    }
    db.execute_batch("DROP TRIGGER fail_second_season").unwrap();
    drain(&store, &manager, &clock, &rows).await;
    assert_eq!(
        store
            .get_probe_job("atomic-seasons")
            .unwrap()
            .unwrap()
            .status,
        "succeeded"
    );
    for row in &rows {
        assert!(
            store
                .get_file_marker(row.file.file_id, row.units[0])
                .unwrap()
                .is_some()
        );
    }
    manager.shutdown_workers().await.unwrap();
}
