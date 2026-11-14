use super::*;

#[test]
fn changed_transfer_setting_is_rejected_before_reservation() {
    let (tmp, state, media, subscribe) = fixture();
    state.store.set_transfer_mode("copy").unwrap();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    state.store.set_transfer_mode("move").unwrap();
    let fingerprint = state
        .store
        .read_fingerprint(&plan.library_id, media.id)
        .unwrap();
    assert_eq!(
        fingerprint, plan.fingerprint,
        "settings evade the scoped fingerprint"
    );
    let recorder = PlannedPublicationRecorder::new(
        PublicationRecorder {
            state: &state,
            subscribe: &subscribe,
            lease: &lease,
            fingerprint,
            snapshot: state
                .store
                .subscribe_library_snapshot(&subscribe, media.kind)
                .unwrap(),
        },
        &plan,
    );
    let source = tmp.path().join("source.mkv");
    let candidate_cov = candidate(&media, &plan, &source, library::TransferMode::Copy);
    let result = subscribe::TransferRecorder::prepare(
        &recorder,
        &candidate_cov,
        &source,
        library::TransferMode::Copy,
    );
    assert!(
        result.is_err(),
        "obsolete mode must not be reserved: {result:?}"
    );
    assert!(state.store.list_file_coverage().unwrap().is_empty());
    assert!(!candidate_cov.file.path.exists());
}

#[test]
fn changed_saved_facts_reject_transfer_reservation() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let recorder = recorder(&state, &subscribe, &lease, &plan);
    let mut facts = domain::SubscribeFacts::default();
    facts.replace(
        None,
        None,
        domain::QualityFact {
            score: 100,
            path: Some(plan.root.join("existing.mkv").display().to_string()),
        },
    );
    state
        .store
        .save_subscribe_facts(subscribe.id, &facts)
        .unwrap();
    let source = tmp.path().join("source.mkv");
    let candidate = candidate(&media, &plan, &source, library::TransferMode::Copy);
    assert!(
        subscribe::TransferRecorder::prepare(
            &recorder,
            &candidate,
            &source,
            library::TransferMode::Copy
        )
        .is_err()
    );
    assert!(state.store.list_file_coverage().unwrap().is_empty());
}

#[test]
fn changed_other_library_rejects_transfer_reservation() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let recorder = recorder(&state, &subscribe, &lease, &plan);
    let nested = plan.root.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    state
        .store
        .create_library(
            media.kind,
            "Nested",
            &[nested.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let source = tmp.path().join("source.mkv");
    let candidate = candidate(&media, &plan, &source, library::TransferMode::Copy);
    assert!(
        subscribe::TransferRecorder::prepare(
            &recorder,
            &candidate,
            &source,
            library::TransferMode::Copy
        )
        .is_err()
    );
    assert!(state.store.list_file_coverage().unwrap().is_empty());
}

#[test]
fn same_transfer_round_can_reserve_multiple_files() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let recorder = recorder(&state, &subscribe, &lease, &plan);
    for name in ["first.mkv", "second.mkv"] {
        let source = tmp.path().join(name);
        let mut candidate = candidate(&media, &plan, &source, library::TransferMode::Copy);
        candidate.file.path = plan.root.join(name);
        subscribe::TransferRecorder::prepare(
            &recorder,
            &candidate,
            &source,
            library::TransferMode::Copy,
        )
        .unwrap();
    }
    assert_eq!(state.store.list_file_coverage().unwrap().len(), 2);
}

#[test]
fn library_publication_does_not_hold_app_writer() {
    let (_tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let state = &state;
        let plan = &plan;
        let lease = &lease;
        scope.spawn(move || {
            with_plan_write(
                &state.store,
                &lease,
                Some(&plan.fingerprint),
                &std::collections::HashSet::new(),
                &plan,
                |_db| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                },
            )
            .unwrap();
        });
        entered_rx.recv().unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        scope.spawn(move || {
            state
                .store
                .put_setting("independent.app.write", "saved")
                .unwrap();
            done_tx.send(()).unwrap();
        });
        let independent = done_rx.recv_timeout(std::time::Duration::from_secs(2));
        let (config_tx, config_rx) = std::sync::mpsc::channel();
        scope.spawn(move || {
            let saved = state.store.save_raw_scrape_settings("{}");
            config_tx.send(saved).unwrap();
        });
        let config_waited = config_rx.recv_timeout(std::time::Duration::from_millis(100));
        release_tx.send(()).unwrap();
        assert!(
            config_waited.is_err(),
            "configuration escaped the publication transaction"
        );
        config_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(
            independent.is_ok(),
            "Library publication blocked an unrelated App write"
        );
    });
    assert_eq!(
        state
            .store
            .get_setting("independent.app.write")
            .unwrap()
            .as_deref(),
        Some("saved")
    );
}

#[test]
fn scrape_aba_rejects_reservation_even_when_raw_value_is_restored() {
    let (tmp, state, media, subscribe) = fixture();
    state.store.save_raw_scrape_settings("{}").unwrap();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let recorder = recorder(&state, &subscribe, &lease, &plan);
    state
        .store
        .save_raw_scrape_settings(r#"{"mirror_nfo":true}"#)
        .unwrap();
    state.store.save_raw_scrape_settings("{}").unwrap();
    let source = tmp.path().join("source.mkv");
    let candidate = candidate(&media, &plan, &source, library::TransferMode::Copy);
    assert!(
        subscribe::TransferRecorder::prepare(
            &recorder,
            &candidate,
            &source,
            library::TransferMode::Copy
        )
        .is_err()
    );
    assert!(state.store.list_file_coverage().unwrap().is_empty());
}

#[test]
fn config_change_after_placement_keeps_reservation_pending() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let recorder = recorder(&state, &subscribe, &lease, &plan);
    let source = tmp.path().join("source.mkv");
    let candidate = candidate(&media, &plan, &source, library::TransferMode::Copy);
    let mut reserved = subscribe::TransferRecorder::prepare(
        &recorder,
        &candidate,
        &source,
        library::TransferMode::Copy,
    )
    .unwrap();
    std::fs::create_dir_all(reserved.file.path.parent().unwrap()).unwrap();
    std::fs::write(&reserved.file.path, b"placed bytes").unwrap();
    state.store.save_raw_scrape_settings("{}").unwrap();
    reserved.file.publication = domain::FilePublication::Published;
    assert!(subscribe::TransferRecorder::published(&recorder, &reserved).is_err());
    let files = state.store.list_file_coverage().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].file.file_id, reserved.file.file_id);
    assert_eq!(files[0].file.publication, domain::FilePublication::Pending);
    assert_eq!(std::fs::read(&reserved.file.path).unwrap(), b"placed bytes");
}

#[test]
fn public_reservation_succeeds_while_app_writer_is_occupied() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let recorder = recorder(&state, &subscribe, &lease, &plan);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let state = &state;
        scope.spawn(move || {
            state
                .store
                .with_writer(store::Db::App, |_| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                })
                .unwrap();
        });
        entered_rx.recv().unwrap();
        let source = tmp.path().join("source.mkv");
        let candidate = candidate(&media, &plan, &source, library::TransferMode::Copy);
        let reserved = subscribe::TransferRecorder::prepare(
            &recorder,
            &candidate,
            &source,
            library::TransferMode::Copy,
        );
        release_tx.send(()).unwrap();
        assert!(
            reserved.is_ok(),
            "App writer blocked reservation: {reserved:?}"
        );
    });
    assert_eq!(state.store.list_file_coverage().unwrap().len(), 1);
}

#[test]
fn transfer_aba_rejects_reservation_when_mode_returns_to_original() {
    let (tmp, state, media, subscribe) = fixture();
    state.store.set_transfer_mode("copy").unwrap();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let recorder = recorder(&state, &subscribe, &lease, &plan);
    state.store.set_transfer_mode("move").unwrap();
    state.store.set_transfer_mode("copy").unwrap();
    let source = tmp.path().join("source.mkv");
    let candidate = candidate(&media, &plan, &source, library::TransferMode::Copy);
    assert!(
        subscribe::TransferRecorder::prepare(
            &recorder,
            &candidate,
            &source,
            library::TransferMode::Copy,
        )
        .is_err()
    );
    assert!(state.store.list_file_coverage().unwrap().is_empty());
}
