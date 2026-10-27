use super::*;

#[test]
fn changed_transfer_setting_is_rejected_before_reservation() {
    let (tmp, state, media, subscribe) = fixture();
    state.store.lock().set_transfer_mode("copy").unwrap();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    state.store.lock().set_transfer_mode("move").unwrap();
    let fingerprint = state
        .store
        .lock()
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
                .lock()
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
    assert!(state.store.lock().list_file_coverage().unwrap().is_empty());
    assert!(!candidate_cov.file.path.exists());
}

#[test]
fn changed_saved_facts_reject_transfer_reservation() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let recorder = recorder(&state, &subscribe, &lease, &plan);
    let mut facts = subscribe::SubscribeFacts::default();
    facts.replace(
        None,
        None,
        subscribe::QualityFact {
            score: 100,
            path: Some(plan.root.join("existing.mkv").display().to_string()),
        },
    );
    state
        .store
        .lock()
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
    assert!(state.store.lock().list_file_coverage().unwrap().is_empty());
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
        .lock()
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
    assert!(state.store.lock().list_file_coverage().unwrap().is_empty());
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
    assert_eq!(state.store.lock().list_file_coverage().unwrap().len(), 2);
}
