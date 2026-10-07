#[test]
fn force_probe_during_existing_probe_is_rejected_until_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let manager = ProbeManager::new(store);
    let unit = ProbeUnit {
        row: LedgerRow {
            id: LedgerId::new(),
            media_id: MediaId::new(),
            path: "movie.mkv".into(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        },
        kind: MediaKind::Movie,
        force_fingerprint: false,
        overwrite_markers: false,
        marker_refresh_id: None,
        job_id: None,
    };
    let id = unit.row.id.to_string();
    assert!(manager.enqueue(unit.clone()));
    assert!(!manager.enqueue_force(unit.clone()));
    let mut rx = manager.rx.try_lock().unwrap();
    let queued = rx.try_recv().unwrap();
    assert_eq!(queued.row.id, unit.row.id);
    assert!(rx.try_recv().is_err());
    manager.finish(&queued, true);
    assert!(
        rx.try_recv().is_err(),
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
    assert_eq!(rx.try_recv().unwrap().row.id.to_string(), id);
}

#[test]
fn stale_queue_message_cannot_start_or_finish_a_later_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let manager = ProbeManager::new(store.clone());
    let unit = ProbeUnit {
        row: LedgerRow {
            id: LedgerId::new(),
            media_id: MediaId::new(),
            path: "retry.mkv".into(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        },
        kind: MediaKind::Movie,
        force_fingerprint: false,
        overwrite_markers: false,
        marker_refresh_id: None,
        job_id: None,
    };
    store.lock().insert_ledger(&unit.row).unwrap();

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
            .start_probe_unit(&stale_job, &unit.row.id.to_string())
            .unwrap()
    );
    manager.finish(&stale, true);
    let active = store
        .lock()
        .active_probe_unit_for_ledger(&unit.row.id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(active.job_id, retry_job);
    assert_eq!(active.status, "queued");
}

#[test]
fn queued_probe_is_recovered_after_manager_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let unit = ProbeUnit {
        row: LedgerRow {
            id: LedgerId::new(),
            media_id: MediaId::new(),
            path: "episode.mkv".into(),
            season: Some(1),
            episode: Some(1),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        },
        kind: MediaKind::Tv,
        force_fingerprint: false,
        overwrite_markers: false,
        marker_refresh_id: None,
        job_id: None,
    };
    let ledger_id = unit.row.id.to_string();
    store.lock().insert_ledger(&unit.row).unwrap();
    let manager = ProbeManager::new(store.clone());

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

    let restarted = ProbeManager::new(store.clone());
    assert!(restarted.is_queued(&ledger_id));
    assert_eq!(
        restarted.take_queued_for_test().unwrap().row.id,
        unit.row.id,
        "a running database unit must be requeued after a process restart"
    );
}

#[test]
fn completed_marker_refresh_with_missing_ledger_becomes_failed_on_recovery() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let media_id = MediaId::new();
    let job_id = "completed-marker-refresh";
    let units = [crate::store::ProbeJobUnitSpec {
        ledger_id: "removed-ledger-row",
        kind: "tv",
        force_fingerprint: true,
        overwrite_markers: true,
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
    store
        .lock()
        .start_probe_unit(job_id, "removed-ledger-row")
        .unwrap();
    store
        .lock()
        .finish_probe_unit(job_id, "removed-ledger-row", true, None)
        .unwrap();
    assert_eq!(
        store.lock().get_probe_job(job_id).unwrap().unwrap().status,
        "running"
    );

    let _restarted = ProbeManager::new(store.clone());
    let recovered = store.lock().get_probe_job(job_id).unwrap().unwrap();
    assert_eq!(recovered.status, "failed");
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
    let manager = ProbeManager::new(store.clone());
    let unit = ProbeUnit {
        row: LedgerRow {
            id: LedgerId::new(),
            media_id: MediaId::new(),
            path: "failed-episode.mkv".into(),
            season: Some(1),
            episode: Some(1),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        },
        kind: MediaKind::Tv,
        force_fingerprint: false,
        overwrite_markers: false,
        marker_refresh_id: None,
        job_id: None,
    };
    let ledger_id = unit.row.id.to_string();
    store.lock().insert_ledger(&unit.row).unwrap();

    assert!(manager.enqueue(unit.clone()));
    let queued = manager.take_queued_for_test().unwrap();
    assert_eq!(queued.row.id, unit.row.id);
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
    let rows = [1, 2].map(|episode| LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: format!("episode-{episode}.mkv"),
        season: Some(1),
        episode: Some(episode),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    });
    for row in &rows {
        store.lock().insert_ledger(row).unwrap();
    }
    let units = || {
        rows.iter()
            .map(|row| ProbeUnit {
                row: row.clone(),
                kind: MediaKind::Tv,
                force_fingerprint: true,
                overwrite_markers: true,
                marker_refresh_id: None,
                job_id: None,
            })
            .collect()
    };
    let first = ProbeManager::new(store.clone());
    let second = ProbeManager::new(store.clone());

    assert_eq!(first.enqueue_marker_refresh(units()), 2);
    let scope = super::super::marker_refresh_scope(media_id, 1);
    let active = store
        .lock()
        .active_probe_job_for_scope(&scope)
        .unwrap()
        .unwrap();
    assert_eq!(active.total, 2);
    assert_eq!(active.completed, 0);
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
    let rows = [1, 2].map(|episode| LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: format!("episode-{episode}.mkv"),
        season: Some(1),
        episode: Some(episode),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    });
    for row in &rows {
        store.lock().insert_ledger(row).unwrap();
    }
    let manager = ProbeManager::new(store.clone());
    let active_unit = ProbeUnit {
        row: rows[1].clone(),
        kind: MediaKind::Tv,
        force_fingerprint: false,
        overwrite_markers: false,
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
            overwrite_markers: false,
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
    assert_eq!(manager.take_queued_for_test().unwrap().row.id, rows[1].id);
    assert!(manager.take_queued_for_test().is_none());
}

#[test]
fn forced_tv_voiceprint_ignores_library_fingerprint_toggle() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let manager = ProbeManager::new(store);

    assert_eq!(
        fingerprint_duration(
            &manager,
            &PathBuf::from("/unconfigured-library/Show.S01E01.mkv"),
            MediaKind::Tv,
            true,
        ),
        Some(180),
        "an explicit refresh must run voiceprint analysis even when the opt-in toggle is off"
    );
    assert_eq!(
        fingerprint_duration(
            &manager,
            &PathBuf::from("/unconfigured-library/movie.mkv"),
            MediaKind::Movie,
            true,
        ),
        None,
        "voiceprint extraction remains TV-only"
    );
}
