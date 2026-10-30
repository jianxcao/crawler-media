fn seed_episode_old_refresh_result(
    store: &Store,
    media_id: MediaId,
    ledger_id: &str,
    episode: u32,
) -> Vec<marker::ChapterMarker> {
    store
        .put_media_marker(&crate::store::StoredMediaMarker {
            media_id,
            season: 1,
            episode,
            intro_start_ms: Some(111_000),
            intro_end_ms: Some(222_000),
            outro_start_ms: Some(1_000_000),
            outro_end_ms: Some(1_050_000),
            source: "old-result".into(),
            locked: false,
            updated_at: 0,
        })
        .unwrap();
    let cached = vec![
        marker::ChapterMarker {
            start_ms: 0,
            end_ms: 300_000,
            title: Some("第一章".into()),
            marker_type: None,
            synthetic: false,
        },
        marker::ChapterMarker {
            start_ms: 300_000,
            end_ms: 600_000,
            title: Some("第二章".into()),
            marker_type: None,
            synthetic: false,
        },
    ];
    store.put_cached_chapters(ledger_id, &cached).unwrap();

    cached
}
struct RefreshHarness {
    _tmp: tempfile::TempDir,
    store: Arc<Mutex<Store>>,
    registry: Arc<crate::library_lifecycle::LibraryWorkRegistry>,
    manager: Arc<ProbeManager>,
    media_id: MediaId,
    library_id: String,
    rows: Vec<domain::FileCoverage>,
    expected_chapters: Vec<Vec<marker::ChapterMarker>>,
    scope: String,
}

impl RefreshHarness {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let lib_dir = tmp.path().join("tv_lib");
        std::fs::create_dir_all(&lib_dir).unwrap();
        let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
        let media_id = MediaId::new();
        let library = store
            .lock()
            .create_library(
                MediaKind::Tv,
                "TV",
                &[lib_dir.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store
            .lock()
            .set_library_intro_settings(&library.id, true, true)
            .unwrap();
        store
            .lock()
            .insert_media(&domain::Media {
                id: media_id,
                kind: MediaKind::Tv,
                title: "万神殿".into(),
                year: None,
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let make_row = |episode| {
            super::super::test_support::coverage(
                media_id,
                &lib_dir
                    .join(format!("Pantheon.S01E{episode:02}.mkv"))
                    .to_string_lossy()
                    .into_owned(),
                Some(1),
                Some(episode),
            )
        };
        let mut rows = vec![make_row(1), make_row(2)];
        for row in rows.iter_mut() {
            std::fs::write(&row.file.path, b"dummy").unwrap();
            super::super::test_support::publish_into(&store.lock(), row);
        }
        let expected_chapters = {
            let store = store.lock();
            vec![
                seed_episode_old_refresh_result(
                    &store,
                    media_id,
                    &rows[0].file.file_id.to_string(),
                    1,
                ),
                seed_episode_old_refresh_result(
                    &store,
                    media_id,
                    &rows[1].file.file_id.to_string(),
                    2,
                ),
            ]
        };

        let registry = Arc::new(crate::library_lifecycle::LibraryWorkRegistry::default());
        let manager = Arc::new(ProbeManager::new(store.clone()).unwrap());
        manager.set_library_registry(registry.clone());
        let scope = format!("marker-refresh:{media_id}:all");

        Self {
            _tmp: tmp,
            store,
            registry,
            manager,
            media_id,
            library_id: library.id,
            rows,
            expected_chapters,
            scope,
        }
    }

    fn make_units(&self) -> Vec<ProbeUnit> {
        self.rows
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
            .collect()
    }

    fn assert_old_markers_intact(&self) {
        let store = self.store.lock();
        for (idx, row) in self.rows.iter().enumerate() {
            let marker = store
                .get_media_marker(
                    self.media_id,
                    super::super::single_season_opt(row),
                    super::super::single_episode_opt(row),
                )
                .unwrap()
                .expect("旧标记事实必须保持可见");
            assert_eq!(marker.source, "old-result");
            assert_eq!(marker.intro_start_ms, Some(111_000));
            assert_eq!(marker.intro_end_ms, Some(222_000));
            assert_eq!(marker.outro_start_ms, Some(1_000_000));
            assert_eq!(marker.outro_end_ms, Some(1_050_000));
            let chapters = store
                .get_cached_chapters(&row.file.file_id.to_string())
                .unwrap()
                .expect("旧章节缓存必须存在");
            assert_eq!(chapters, self.expected_chapters[idx]);
            assert!(chapters.iter().all(|c| c.marker_type.is_none()));
        }
    }
}

struct PresenceRaceTracks;

struct PresenceRaceFingerprint;

impl marker::FingerprintEngine for PresenceRaceFingerprint {
    fn extract_at(
        &self,
        _path: &std::path::Path,
        _start_secs: u32,
        _duration_secs: u32,
        _control: &marker::WorkControl,
    ) -> Result<marker::AudioFingerprint, String> {
        Ok(vec![1, 2, 3])
    }

    fn find_common_segment(
        &self,
        _first: &[u32],
        _second: &[u32],
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Option<marker::CommonSegment> {
        None
    }
}

impl super::super::TracksProbe for PresenceRaceTracks {
    fn probe(
        &self,
        _path: &std::path::Path,
        _control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        Ok((library::Tracks::default(), Some(1_000)))
    }
}

#[test]
fn restored_presence_does_not_revive_cancelled_probe_unit() {
    let (done, completion) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = std::panic::catch_unwind(|| runtime.block_on(check_restored_presence()));
        done.send(result).unwrap();
    });
    completion
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("presence regression timed out")
        .unwrap();
    worker.join().unwrap();
}

async fn check_restored_presence() {
    let harness = RefreshHarness::new();
    let manager =
        ProbeManager::new(harness.store.clone()).unwrap().with_tracks_probe(Arc::new(PresenceRaceTracks));
    let ids: Vec<String> = harness
        .rows
        .iter()
        .map(|row| row.file.file_id.to_string())
        .collect();
    let specs: Vec<_> = ids
        .iter()
        .map(|id| store::ProbeJobUnitSpec {
            ledger_id: id,
            kind: "tv",
            force_fingerprint: false,
            reuse_fingerprint_cache: false,
            overwrite_markers: false,
            reuse_media_info_cache: false,
        })
        .collect();
    let mut units = harness.make_units();
    for unit in &mut units {
        unit.job_id = Some("presence-race".into());
        unit.reuse_media_info_cache = false;
    }
    {
        let store = harness.store.lock();
        assert!(
            store
                .create_probe_job(
                    "presence-race",
                    "media_probe",
                    &harness.media_id.to_string(),
                    None,
                    "presence-race",
                    &specs,
                )
                .unwrap()
        );
        for id in &ids {
            assert!(store.start_probe_unit("presence-race", id).unwrap());
        }
        let current = store
            .file_coverage_by_id(harness.rows[0].file.file_id)
            .unwrap()
            .unwrap();
        let observation = store::LedgerPresenceObservation {
            path_key: current.path_key.clone(),
            file: current,
            missing_at: None,
            presence: domain::FilePresence::Missing,
        };
        assert_eq!(
            store
                .apply_ledger_presence(&[observation], store::LedgerPresenceMode::Verify, 1000,)
                .unwrap()
                .missing,
            1
        );
        let current = store
            .file_coverage_by_id(harness.rows[0].file.file_id)
            .unwrap()
            .unwrap();
        let observation = store::LedgerPresenceObservation {
            path_key: current.path_key.clone(),
            file: current,
            missing_at: Some(1000),
            presence: domain::FilePresence::Present,
        };
        assert_eq!(
            store
                .apply_ledger_presence(&[observation], store::LedgerPresenceMode::Verify, 1001,)
                .unwrap()
                .restored,
            1
        );
        assert!(
            store
                .get_probe_job("presence-race")
                .unwrap()
                .unwrap()
                .is_active()
        );
    }
    assert!(!worker::source_ready(&harness.manager, &units[0]));
    assert!(worker::source_ready(&harness.manager, &units[1]));
    assert!(
        super::super::tracks::probe_tracks_and_store(
            &manager,
            &units[0],
            &units[0].row.file.version.source_version,
        )
        .await
        .is_err()
    );
    assert_eq!(
        harness
            .store
            .lock()
            .file_coverage_by_id(units[0].row.file.file_id)
            .unwrap()
            .unwrap()
            .file
            .duration_ms,
        None
    );
    assert!(
        super::super::tracks::probe_tracks_and_store(
            &manager,
            &units[1],
            &units[1].row.file.version.source_version,
        )
        .await
        .is_ok()
    );
    assert_eq!(
        harness
            .store
            .lock()
            .file_coverage_by_id(units[1].row.file.file_id)
            .unwrap()
            .unwrap()
            .file
            .duration_ms,
        Some(1_000)
    );
    let outcome = super::super::probe::probe_fingerprint_and_store_detailed(
        &manager,
        &units[0],
        units[0].row.file.path.clone(),
        60,
        None,
        units[0].row.file.version.source_version.clone(),
        Arc::new(PresenceRaceFingerprint),
        false,
        false,
    )
    .await;
    assert!(matches!(
        outcome,
        super::super::stages::FingerprintRunOutcome::Cancelled { .. }
    ));
    assert!(
        harness
            .store
            .lock()
            .get_fingerprint_cache(&ids[0])
            .unwrap()
            .is_none()
    );
    assert!(super::super::markers::compare_and_store_markers(&manager, &units[0]).is_err());
    harness.assert_old_markers_intact();
}

#[test]
fn window1_queued_stage_cancellation_releases_scope() {
    let harness = RefreshHarness::new();
    assert_eq!(
        harness
            .manager
            .enqueue_forced_item_refresh(harness.make_units())
            .unwrap(),
        2
    );

    let active_job = harness
        .store
        .lock()
        .active_probe_job_for_scope(&harness.scope)
        .unwrap()
        .unwrap();

    harness
        .manager
        .cancel_ledger_work(&[harness.rows[0].file.file_id.to_string()], "user_abort")
        .unwrap();

    let job = harness
        .store
        .lock()
        .get_probe_job(&active_job.id)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, "cancelled");
    assert!(
        harness
            .store
            .lock()
            .active_probe_job_for_scope(&harness.scope)
            .unwrap()
            .is_none()
    );
    harness.assert_old_markers_intact();

    let first = harness.manager.take_queued_for_test().unwrap();
    assert!(!worker::source_ready(&harness.manager, &first));
    assert!(
        !harness
            .manager
            .seen
            .lock()
            .contains(&first.row.file.file_id.to_string())
    );
}

#[test]
fn window3_pre_publication_freeze_prevents_late_result() {
    let harness = RefreshHarness::new();
    assert_eq!(
        harness
            .manager
            .enqueue_forced_item_refresh(harness.make_units())
            .unwrap(),
        2
    );
    let job_id = harness
        .store
        .lock()
        .active_probe_job_for_scope(&harness.scope)
        .unwrap()
        .unwrap()
        .id;

    let first = harness.manager.take_queued_for_test().unwrap();
    let second = harness.manager.take_queued_for_test().unwrap();

    harness.manager.finish(&first, true);
    assert_eq!(
        harness
            .store
            .lock()
            .get_probe_job(&job_id)
            .unwrap()
            .unwrap()
            .completed,
        1
    );

    harness
        .store
        .lock()
        .begin_library_removal(&harness.library_id)
        .unwrap();
    harness.registry.cancel(&harness.library_id);

    harness.manager.finish(&second, true);

    let job = harness
        .store
        .lock()
        .get_probe_job(&job_id)
        .unwrap()
        .unwrap();
    assert_ne!(job.status, "succeeded");
    assert_eq!(job.status, "cancelled");
    assert_eq!(job.error.as_deref(), Some("file_deleted"));
    harness.assert_old_markers_intact();
    assert!(
        harness
            .store
            .lock()
            .active_probe_job_for_scope(&harness.scope)
            .unwrap()
            .is_none()
    );
    assert!(!harness.registry.has_active_work());
}

#[test]
fn window3_pre_publication_file_deleted_prevents_late_result() {
    let harness = RefreshHarness::new();
    assert_eq!(
        harness
            .manager
            .enqueue_forced_item_refresh(harness.make_units())
            .unwrap(),
        2
    );
    let job_id = harness
        .store
        .lock()
        .active_probe_job_for_scope(&harness.scope)
        .unwrap()
        .unwrap()
        .id;

    let first = harness.manager.take_queued_for_test().unwrap();
    let second = harness.manager.take_queued_for_test().unwrap();

    harness.manager.finish(&first, true);

    std::fs::remove_file(&harness.rows[0].file.path).unwrap();

    harness.manager.finish(&second, true);

    let job = harness
        .store
        .lock()
        .get_probe_job(&job_id)
        .unwrap()
        .unwrap();
    assert_ne!(job.status, "succeeded");
    assert_eq!(job.status, "cancelled");
    assert_eq!(job.error.as_deref(), Some("file_deleted"));
    harness.assert_old_markers_intact();
    assert!(
        harness
            .store
            .lock()
            .active_probe_job_for_scope(&harness.scope)
            .unwrap()
            .is_none()
    );
}

#[test]
fn window4_recovery_interrupted_with_frozen_library_cancels_cleanly() {
    let harness = RefreshHarness::new();
    assert_eq!(
        harness
            .manager
            .enqueue_forced_item_refresh(harness.make_units())
            .unwrap(),
        2
    );
    let job_id = harness
        .store
        .lock()
        .active_probe_job_for_scope(&harness.scope)
        .unwrap()
        .unwrap()
        .id;

    harness
        .store
        .lock()
        .begin_library_removal(&harness.library_id)
        .unwrap();

    let restarted = ProbeManager::new(harness.store.clone()).unwrap();
    restarted.recover_pending_jobs();

    let job = harness
        .store
        .lock()
        .get_probe_job(&job_id)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, "cancelled");
    assert_eq!(job.error.as_deref(), Some("Library removed"));
    assert!(!restarted.is_queued(&harness.rows[0].file.file_id.to_string()));
    assert!(
        harness
            .store
            .lock()
            .active_probe_job_for_scope(&harness.scope)
            .unwrap()
            .is_none()
    );
    harness.assert_old_markers_intact();

    restarted.recover_pending_jobs();
    assert_eq!(
        harness
            .store
            .lock()
            .get_probe_job(&job_id)
            .unwrap()
            .unwrap()
            .status,
        "cancelled"
    );
}

fn assert_recovery_cancels_after_member_deletion(deleted_index: usize) {
    let harness = RefreshHarness::new();
    assert_eq!(
        harness
            .manager
            .enqueue_forced_item_refresh(harness.make_units())
            .unwrap(),
        2
    );
    let job_id = harness
        .store
        .lock()
        .active_probe_job_for_scope(&harness.scope)
        .unwrap()
        .unwrap()
        .id;

    harness
        .store
        .lock()
        .finish_probe_unit_with_status(
            &job_id,
            &harness.rows[0].file.file_id.to_string(),
            "succeeded",
            None,
            None,
        )
        .unwrap();
    harness
        .store
        .lock()
        .finish_probe_unit_with_status(
            &job_id,
            &harness.rows[1].file.file_id.to_string(),
            "succeeded",
            None,
            None,
        )
        .unwrap();

    std::fs::remove_file(&harness.rows[deleted_index].file.path).unwrap();

    let restarted = ProbeManager::new(harness.store.clone()).unwrap();
    restarted.recover_pending_jobs();

    let job = harness
        .store
        .lock()
        .get_probe_job(&job_id)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, "cancelled");
    assert_eq!(job.error.as_deref(), Some("file_deleted"));
    assert!(
        harness
            .store
            .lock()
            .active_probe_job_for_scope(&harness.scope)
            .unwrap()
            .is_none()
    );
    harness.assert_old_markers_intact();
}

#[test]
fn window4_recovery_completed_units_with_deleted_source_cancels_job() {
    for deleted_index in [0, 1] {
        assert_recovery_cancels_after_member_deletion(deleted_index);
    }
}
