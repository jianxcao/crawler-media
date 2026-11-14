use super::*;

#[test]
fn cancelled_reconciliation_trigger_transfers_durable_cursor_to_survivor() {
    let f = Fixture::new();
    let first = f.store.prepare_probe_analysis(&f.captured(1)).unwrap();
    let second = f.store.prepare_probe_analysis(&f.captured(2)).unwrap();
    for participant in [&first, &second] {
        assert!(f.store.advance_probe_unit_to_analysis(participant).unwrap());
    }
    let cursor = serde_json::to_string(&store::ProbeAnalysisCursor(
        16,
        "library".into(),
        "file".into(),
    ))
    .unwrap();
    assert!(
        f.store
            .refresh_probe_analysis_targets(&store::ProbeAnalysisReconcile {
                trigger: first.clone(),
                expected_cursor: None,
                reference_key: "current".into(),
                candidates: vec![second.clone()],
                next_cursor: Some(cursor.clone()),
            })
            .unwrap()
    );
    f.store
        .cancel_probe_unit(
            &first.dispatch().job_id,
            &first.dispatch().ledger_id,
            "Library removed",
        )
        .unwrap();
    let state = f
        .store
        .probe_analysis_refresh_state(second.dispatch())
        .unwrap()
        .unwrap();
    assert!(
        !state.reconcile_done,
        "survivor must finish the unscanned reference suffix"
    );
    assert_eq!(state.reconcile_after, Some(cursor.clone()));
    drop(f.store);
    let store = Store::open(f.dir.path()).unwrap();
    assert!(
        store
            .refresh_probe_analysis_targets(&store::ProbeAnalysisReconcile {
                trigger: second,
                expected_cursor: Some(cursor),
                reference_key: "current".into(),
                candidates: vec![],
                next_cursor: None,
            })
            .unwrap()
    );
}

#[test]
fn cancelled_last_trigger_creates_successor_for_terminal_surviving_source() {
    let f = Fixture::new();
    let first = f.store.prepare_probe_analysis(&f.captured(1)).unwrap();
    let second = f.store.prepare_probe_analysis(&f.captured(2)).unwrap();
    for participant in [&first, &second] {
        assert!(f.store.advance_probe_unit_to_analysis(participant).unwrap());
    }
    f.store
        .finish_probe_unit(
            &second.dispatch().job_id,
            &second.dispatch().ledger_id,
            true,
            None,
        )
        .unwrap();
    let cursor = serde_json::to_string(&store::ProbeAnalysisCursor(
        1,
        "library".into(),
        "file".into(),
    ))
    .unwrap();
    assert!(
        f.store
            .refresh_probe_analysis_targets(&store::ProbeAnalysisReconcile {
                trigger: first.clone(),
                expected_cursor: None,
                reference_key: "current".into(),
                candidates: vec![],
                next_cursor: Some(cursor.clone()),
            })
            .unwrap()
    );
    f.store
        .cancel_probe_unit(
            &first.dispatch().job_id,
            &first.dispatch().ledger_id,
            "Library removed",
        )
        .unwrap();
    let successor = f
        .store
        .active_probe_unit_for_ledger(&second.dispatch().ledger_id)
        .unwrap()
        .expect("terminal survivor receives the unfinished obligation");
    assert_ne!(successor.job_id, second.dispatch().job_id);
    assert_eq!(successor.stage, ProbeDispatchStage::Analysis);
    let id = store::ProbeDispatchId {
        job_id: successor.job_id,
        ledger_id: successor.ledger_id,
        stage: ProbeDispatchStage::Analysis,
    };
    let state = f.store.probe_analysis_refresh_state(&id).unwrap().unwrap();
    assert_eq!(state.reconcile_after, Some(cursor));
    assert!(!state.reconcile_done);
}

#[test]
fn cancelling_another_library_reference_rejects_old_computation_but_keeps_target() {
    let f = Fixture::new();
    let path = f.dir.path().join("references");
    let library = f
        .store
        .create_library(
            MediaKind::Tv,
            "references",
            &[path.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let target = f.store.prepare_probe_analysis(&f.captured(1)).unwrap();
    let reference = f
        .store
        .prepare_probe_analysis(&f.captured_in(2, &library))
        .unwrap();
    for participant in [&target, &reference] {
        f.store.advance_probe_unit_to_analysis(participant).unwrap();
    }
    let claim = f
        .store
        .claim_probe_analysis("before-cancel", std::slice::from_ref(&target))
        .unwrap()
        .unwrap();
    let mut commit = store::ProbeAnalysisCommit {
        claim: claim.clone(),
        outputs: vec![store::ProbeAnalysisOutput {
            dispatch: target.dispatch().clone(),
            detection_outcome_json: "{\"status\":\"verified\"}".into(),
            replacement: None,
        }],
        references: vec![target.clone(), reference.clone()],
        observations: [&target, &reference]
            .into_iter()
            .map(|p| store::SourceObservation {
                file_id: p.context().target.file.file_id,
                expected: p.context().target.file.version.clone(),
                presence: domain::FilePresence::Present,
            })
            .collect(),
        models: vec![],
        followups: vec![],
        refresh_job_id: None,
        refresh_error: None,
    };
    f.store
        .set_library_intro_settings(&library.id, false, false)
        .unwrap();
    assert!(
        f.store
            .complete_probe_analysis_checked(&commit)
            .unwrap()
            .is_none(),
        "consumed reference outside outputs was disabled"
    );
    assert!(
        f.store
            .get_probe_job(&target.dispatch().job_id)
            .unwrap()
            .unwrap()
            .is_active()
    );
    f.store.requeue_probe_analysis(&claim).unwrap();
    commit.claim = f
        .store
        .claim_probe_analysis("survivor", std::slice::from_ref(&target))
        .unwrap()
        .unwrap();
    commit.references = vec![target.clone()];
    commit.observations.truncate(1);
    commit.outputs[0].detection_outcome_json = "{\"status\":\"uncertain\"}".into();
    assert!(
        f.store
            .complete_probe_analysis_checked(&commit)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        f.store
            .get_probe_job(&target.dispatch().job_id)
            .unwrap()
            .unwrap()
            .status,
        "succeeded"
    );
}
