/// 在 `tmp` 下建一个 `kind` 库并登记 `media_id`：`library_files` 的
/// (media_id, kind) 外键和路径归属都要求两者先存在，且文件路径必须落在库根内。
fn seed_probe_library(store: &Store, tmp: &tempfile::TempDir, media_id: MediaId, kind: MediaKind) {
    let library = store
        .create_library(
            kind,
            "Fixture",
            &[tmp.path().to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&library.id, true, true)
        .unwrap();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind,
            title: "Fixture".into(),
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
}

#[test]
fn force_probe_during_existing_probe_is_rejected_until_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let manager = ProbeManager::new(store).unwrap();
    let unit = ProbeUnit {
        row: super::super::test_support::coverage(MediaId::new(), "movie.mkv", None, None),
        kind: MediaKind::Movie,
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };
    let id = unit.row.file.file_id.to_string();
    assert!(manager.enqueue(unit.clone()));
    assert!(!manager.enqueue_force(unit.clone()));
    let queued = manager.take_queued_for_test().unwrap();
    assert_eq!(queued.row.file.file_id, unit.row.file.file_id);
    assert!(manager.take_queued_for_test().is_none());
    manager.finish(&queued, true);
    assert!(
        manager.take_queued_for_test().is_none(),
        "duplicate request must not become a hidden rerun"
    );
    assert!(!manager.is_queued(&id));
    manager.mark_failed(&id);
    assert!(
        !manager.enqueue(unit.clone()),
        "automatic retry must respect cooldown"
    );
    assert!(
        manager.enqueue_force(unit),
        "manual retry bypasses cooldown"
    );
    assert_eq!(
        manager
            .take_queued_for_test()
            .unwrap()
            .row
            .file
            .file_id
            .to_string(),
        id
    );
}

#[test]
fn stale_queue_message_cannot_start_or_finish_a_later_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let manager = ProbeManager::new(store.clone()).unwrap();
    let mut unit = ProbeUnit {
        row: super::super::test_support::coverage(
            MediaId::new(),
            &tmp.path().join("retry.mkv").display().to_string(),
            None,
            None,
        ),
        kind: MediaKind::Movie,
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };
    seed_probe_library(
        &store.lock(),
        &tmp,
        unit.row.file.media_id,
        MediaKind::Movie,
    );
    super::super::test_support::publish_into(&store.lock(), &mut unit.row);

    assert!(manager.enqueue(unit.clone()));
    let stale = manager.take_queued_for_test().unwrap();
    let stale_job = stale.job_id.clone().unwrap();
    manager.finish(&stale, false);
    assert!(manager.enqueue_force(unit.clone()));
    let retry = manager.take_queued_for_test().unwrap();
    let retry_job = retry.job_id.clone().unwrap();
    assert_ne!(stale_job, retry_job);

    assert!(
        !store
            .lock()
            .start_probe_unit(&stale_job, &unit.row.file.file_id.to_string())
            .unwrap()
    );
    manager.finish(&stale, true);
    let active = store
        .lock()
        .active_probe_unit_for_ledger(&unit.row.file.file_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(active.job_id, retry_job);
    assert_eq!(active.status, "queued");
}

#[test]
fn queued_probe_is_recovered_after_manager_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let mut unit = ProbeUnit {
        row: super::super::test_support::coverage(
            MediaId::new(),
            &tmp.path().join("episode.mkv").display().to_string(),
            Some(1),
            Some(1),
        ),
        kind: MediaKind::Tv,
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };
    let ledger_id = unit.row.file.file_id.to_string();
    seed_probe_library(&store.lock(), &tmp, unit.row.file.media_id, MediaKind::Tv);
    super::super::test_support::publish_into(&store.lock(), &mut unit.row);
    let manager = ProbeManager::new(store.clone()).unwrap();

    assert!(manager.enqueue(unit.clone()));
    assert!(manager.is_queued(&ledger_id));
    let persisted = store
        .lock()
        .active_probe_unit_for_ledger(&ledger_id)
        .unwrap()
        .unwrap();
    assert!(
        store
            .lock()
            .start_probe_unit(&persisted.job_id, &ledger_id)
            .unwrap()
    );
    drop(manager);

    let restarted = ProbeManager::new(store.clone()).unwrap();
    assert!(restarted.is_queued(&ledger_id));
    assert_eq!(
        restarted
            .take_queued_for_test()
            .expect("recovered unit")
            .row
            .file
            .file_id,
        unit.row.file.file_id,
        "a running database unit must be requeued after a process restart"
    );
}

#[test]
fn intake_probe_waits_in_metadata_queue_before_voiceprint_queue() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let manager = ProbeManager::new(store).unwrap();
    let unit = ProbeUnit {
        row: super::super::test_support::coverage(MediaId::new(), "episode.mkv", Some(1), Some(1)),
        kind: MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };

    assert!(manager.enqueue(unit.clone()));
    let metadata_unit = manager.take_queued_for_test().unwrap();
    assert_eq!(metadata_unit.row.file.file_id, unit.row.file.file_id);
    assert!(!manager.has_fingerprint_queued_for_test());
}

#[tokio::test]
async fn voiceprint_worker_yields_until_metadata_backlog_is_done() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let manager = Arc::new(ProbeManager::new(store).unwrap());
    let unit = ProbeUnit {
        row: super::super::test_support::coverage(MediaId::new(), "episode.mkv", Some(1), Some(1)),
        kind: MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };

    assert!(manager.enqueue(unit));
    let _metadata_unit = manager.take_queued_for_test().unwrap();
    let waiter_manager = manager.clone();
    let waiter = tokio::spawn(async move {
        super::super::worker::wait_for_metadata_idle(&waiter_manager).await;
    });
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "voiceprint must wait while metadata is pending"
    );

    manager.metadata_stage_finished();
    waiter.await.unwrap();
}

#[test]
fn metadata_job_finishes_before_a_separate_voiceprint_job_is_queued() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let manager = ProbeManager::new(store.clone()).unwrap();
    let mut unit = ProbeUnit {
        row: super::super::test_support::coverage(
            MediaId::new(),
            &tmp.path().join("episode.mkv").display().to_string(),
            Some(1),
            Some(1),
        ),
        kind: MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: true,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };
    seed_probe_library(&store.lock(), &tmp, unit.row.file.media_id, MediaKind::Tv);
    super::super::test_support::publish_into(&store.lock(), &mut unit.row);

    assert!(manager.enqueue(unit));
    let metadata_unit = manager.take_queued_for_test().unwrap();
    let metadata_job_id = metadata_unit.job_id.clone().unwrap();
    let ledger_id = metadata_unit.row.file.file_id.to_string();
    assert!(
        store
            .lock()
            .start_probe_unit(&metadata_job_id, &ledger_id)
            .unwrap()
    );
    manager.metadata_stage_finished();
    assert!(
        manager.enqueue_fingerprint_after_metadata(super::super::probe::FingerprintWork {
            unit: metadata_unit,
            media_duration_ms: Some(3_000_000),
            source_version: "source-v1".into(),
            duration_secs: 180,
            start_job: false,
        })
    );

    let metadata_job = store
        .lock()
        .get_probe_job(&metadata_job_id)
        .unwrap()
        .unwrap();
    assert_eq!(metadata_job.kind, "media_probe");
    assert_eq!(metadata_job.status, "succeeded");
    let active_voiceprint = store
        .lock()
        .active_probe_unit_for_ledger(&ledger_id)
        .unwrap();
    let active_voiceprint = active_voiceprint.unwrap();
    assert_eq!(active_voiceprint.status, "queued");
    let voiceprint_job = store
        .lock()
        .get_probe_job(&active_voiceprint.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(voiceprint_job.kind, "fingerprint_probe");
    assert_eq!(voiceprint_job.status, "queued");
    let voiceprint_job_id = voiceprint_job.id;

    drop(manager);
    let restarted = ProbeManager::new(store.clone()).unwrap();
    assert!(restarted.is_queued(&ledger_id));
    let recovered = restarted.take_fingerprint_for_test().unwrap();
    assert_eq!(
        recovered.job_id.as_deref(),
        Some(voiceprint_job_id.as_str())
    );
    assert!(restarted.take_queued_for_test().is_none());
    restarted.finish(&recovered, false);
    assert_eq!(
        store
            .lock()
            .get_probe_job(&metadata_job_id)
            .unwrap()
            .unwrap()
            .status,
        "succeeded",
        "a failed optional voiceprint must not undo completed media information"
    );
    assert_eq!(
        store
            .lock()
            .get_probe_job(&voiceprint_job_id)
            .unwrap()
            .unwrap()
            .status,
        "failed"
    );
}

#[test]
fn interrupted_refresh_cancellation_is_completed_during_recovery() {
    for has_pending_unit in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
        let media_id = MediaId::new();
        let mut row = super::super::test_support::coverage(
            media_id,
            &tmp.path()
                .join("episode.mkv")
                .to_string_lossy()
                .into_owned(),
            Some(1),
            Some(1),
        );
        std::fs::write(&row.file.path, b"dummy").unwrap();
        seed_probe_library(&store.lock(), &tmp, media_id, MediaKind::Tv);
        super::super::test_support::publish_into(&store.lock(), &mut row);
        let ledger_id = row.file.file_id.to_string();
        let pending_id = if has_pending_unit {
            let mut pending = super::super::test_support::coverage(
                media_id,
                &tmp.path().join("pending.mkv").to_string_lossy(),
                Some(1),
                Some(2),
            );
            std::fs::write(&pending.file.path, b"finite pending source").unwrap();
            super::super::test_support::publish_into(&store.lock(), &mut pending);
            Some(pending.file.file_id.to_string())
        } else {
            None
        };
        let mut specs = vec![crate::store::ProbeJobUnitSpec {
            ledger_id: &ledger_id,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        }];
        if let Some(pending_id) = pending_id.as_deref() {
            specs.push(crate::store::ProbeJobUnitSpec {
                ledger_id: pending_id,
                kind: "tv",
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: true,
            });
        }
        store
            .lock()
            .create_probe_job(
                "interrupted-cancel",
                "marker_refresh",
                &media_id.to_string(),
                Some(1),
                "interrupted-cancel-scope",
                &specs,
            )
            .unwrap();
        store
            .lock()
            .cancel_probe_unit("interrupted-cancel", &ledger_id, "file_deleted")
            .unwrap();

        let manager = ProbeManager::new(store.clone()).unwrap();

        let job = store
            .lock()
            .get_probe_job("interrupted-cancel")
            .unwrap()
            .unwrap();
        assert_eq!(job.status, "cancelled");
        assert_eq!(job.completed, job.total);
        assert_eq!(job.failed, 0);
        assert!(!manager.is_queued(&ledger_id));
        assert!(
            store
                .lock()
                .active_probe_job_for_scope("interrupted-cancel-scope")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .lock()
                .probe_job_units(&job.id)
                .unwrap()
                .iter()
                .all(|unit| unit.status == "cancelled")
        );
    }
}

#[test]
fn completed_marker_refresh_with_missing_ledger_is_cancelled_on_recovery() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let media_id = MediaId::new();
    seed_probe_library(&store.lock(), &tmp, media_id, MediaKind::Tv);
    let mut row = super::super::test_support::coverage(
        media_id,
        &tmp.path().join("removed.mkv").to_string_lossy(),
        Some(1),
        Some(1),
    );
    std::fs::write(&row.file.path, b"finite source before deletion").unwrap();
    super::super::test_support::publish_into(&store.lock(), &mut row);
    let ledger_id = row.file.file_id.to_string();
    let job_id = "completed-marker-refresh";
    let units = [crate::store::ProbeJobUnitSpec {
        ledger_id: &ledger_id,
        kind: "tv",
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: true,
    }];
    store
        .lock()
        .create_probe_job(
            job_id,
            "marker_refresh",
            &media_id.to_string(),
            None,
            "marker-refresh:recovery-test",
            &units,
        )
        .unwrap();
    store.lock().start_probe_unit(job_id, &ledger_id).unwrap();
    store
        .lock()
        .finish_probe_unit(job_id, &ledger_id, true, None)
        .unwrap();
    assert_eq!(
        store.lock().get_probe_job(job_id).unwrap().unwrap().status,
        "running"
    );

    // Simulate cleanup interrupted before job reconciliation. Remove dependent
    // projections with foreign keys enabled, retaining the completion receipt.
    let database = store
        .lock()
        .db_paths()
        .into_iter()
        .find(|path| path.ends_with("library.db"))
        .unwrap();
    let mut connection = rusqlite::Connection::open(database).unwrap();
    let cleanup = connection.transaction().unwrap();
    cleanup
        .execute(
            "DELETE FROM library_items WHERE preferred_file_id=?1",
            [&ledger_id],
        )
        .unwrap();
    cleanup
        .execute(
            "DELETE FROM library_units WHERE preferred_file_id=?1",
            [&ledger_id],
        )
        .unwrap();
    cleanup
        .execute("DELETE FROM library_files WHERE file_id=?1", [&ledger_id])
        .unwrap();
    cleanup.commit().unwrap();
    assert!(
        store
            .lock()
            .file_coverage_by_id(row.file.file_id)
            .unwrap()
            .is_none()
    );
    let _restarted = ProbeManager::new(store.clone()).unwrap();
    let recovered = store.lock().get_probe_job(job_id).unwrap().unwrap();
    assert_eq!(recovered.status, "cancelled");
    assert_eq!(recovered.error.as_deref(), Some("file_deleted"));
    assert_eq!(recovered.completed, recovered.total);
    assert!(recovered.error.is_some());
    assert!(
        store
            .lock()
            .active_probe_job_for_scope("marker-refresh:recovery-test")
            .unwrap()
            .is_none()
    );
}

#[test]
fn failed_probe_is_persisted_and_can_be_retried() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let manager = ProbeManager::new(store.clone()).unwrap();
    let mut unit = ProbeUnit {
        row: super::super::test_support::coverage(
            MediaId::new(),
            &tmp.path().join("failed-episode.mkv").display().to_string(),
            Some(1),
            Some(1),
        ),
        kind: MediaKind::Tv,
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };
    let ledger_id = unit.row.file.file_id.to_string();
    seed_probe_library(&store.lock(), &tmp, unit.row.file.media_id, MediaKind::Tv);
    super::super::test_support::publish_into(&store.lock(), &mut unit.row);

    assert!(manager.enqueue(unit.clone()));
    let queued = manager.take_queued_for_test().unwrap();
    assert_eq!(queued.row.file.file_id, unit.row.file.file_id);
    manager.finish(&queued, false);
    assert!(!manager.is_queued(&ledger_id));
    let scope = format!("ledger:{ledger_id}");
    let failed = store
        .lock()
        .latest_probe_job_for_scope(&scope)
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.failed, 1);

    assert!(manager.enqueue_force(unit));
    let active = store
        .lock()
        .active_probe_job_for_scope(&scope)
        .unwrap()
        .unwrap();
    assert_eq!(active.status, "queued");
}

#[test]
fn active_season_marker_refresh_rejects_a_duplicate_batch() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let media_id = MediaId::new();
    let mut rows = [1, 2].map(|episode| {
        super::super::test_support::coverage(
            media_id,
            &tmp.path()
                .join(format!("episode-{episode}.mkv"))
                .display()
                .to_string(),
            Some(1),
            Some(episode),
        )
    });
    seed_probe_library(&store.lock(), &tmp, media_id, MediaKind::Tv);
    for row in rows.iter_mut() {
        super::super::test_support::publish_into(&store.lock(), row);
    }
    let units = || {
        rows.iter()
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
    };
    let first = ProbeManager::new(store.clone()).unwrap();
    let second = ProbeManager::new(store.clone()).unwrap();

    assert_eq!(first.enqueue_marker_refresh(units()), 2);
    let scope = super::super::marker_refresh_scope(media_id, 1);
    let active = store
        .lock()
        .active_probe_job_for_scope(&scope)
        .unwrap()
        .unwrap();
    assert_eq!(active.total, 2);
    assert_eq!(active.completed, 0);
    assert!(
        store
            .lock()
            .probe_job_units(&active.id)
            .unwrap()
            .iter()
            .all(|unit| !unit.reuse_fingerprint_cache)
    );
    assert_eq!(
        second.enqueue_marker_refresh(units()),
        0,
        "an active refresh for the same Media season must not be queued twice"
    );
}

#[test]
fn forced_item_refresh_reserves_all_episodes_or_none() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let media_id = MediaId::new();
    let mut rows = [1, 2].map(|episode| {
        super::super::test_support::coverage(
            media_id,
            &tmp.path()
                .join(format!("episode-{episode}.mkv"))
                .display()
                .to_string(),
            Some(1),
            Some(episode),
        )
    });
    seed_probe_library(&store.lock(), &tmp, media_id, MediaKind::Tv);
    for row in rows.iter_mut() {
        super::super::test_support::publish_into(&store.lock(), row);
    }
    let manager = ProbeManager::new(store.clone()).unwrap();
    let active_unit = ProbeUnit {
        row: rows[1].clone(),
        kind: MediaKind::Tv,
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };
    assert!(manager.enqueue(active_unit));

    let batch = rows
        .iter()
        .map(|row| ProbeUnit {
            row: row.clone(),
            kind: MediaKind::Tv,
            force_fingerprint: false,
            reuse_fingerprint_cache: false,
            overwrite_markers: false,
            reuse_media_info_cache: true,
            marker_refresh_id: None,
            job_id: None,
        })
        .collect();
    assert!(matches!(
        manager.enqueue_forced_item_refresh(batch),
        Err(super::ProbeEnqueueError::AlreadyRunning)
    ));
    assert!(
        store
            .lock()
            .active_probe_job_for_scope(&format!("marker-refresh:{media_id}:all"))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        manager.take_queued_for_test().unwrap().row.file.file_id,
        rows[1].file.file_id
    );
    assert!(manager.take_queued_for_test().is_none());
}

#[test]
fn explicit_tv_refresh_works_with_switches_off_but_automatic_force_does_not() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let library = store
        .lock()
        .create_library(
            MediaKind::Tv,
            "Manual",
            &[tmp.path().to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let manager = ProbeManager::new(store).unwrap();
    assert_eq!(
        fingerprint_duration(&manager, &library.id, MediaKind::Tv, true),
        Some(180)
    );
    assert_eq!(
        fingerprint_duration(&manager, &library.id, MediaKind::Tv, false),
        None
    );
    assert_eq!(
        fingerprint_duration(&manager, &library.id, MediaKind::Movie, true),
        None
    );
    assert_eq!(
        fingerprint_duration(&manager, "missing", MediaKind::Tv, true),
        None
    );
}
