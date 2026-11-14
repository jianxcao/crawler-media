use super::*;

fn observation(participant: &store::ProbeAnalysisParticipant) -> store::SourceObservation {
    let file = &participant.context().target.file;
    store::SourceObservation {
        file_id: file.file_id,
        expected: file.version.clone(),
        presence: domain::FilePresence::Present,
    }
}

fn assert_unobserved_followup_rejected(store: &Store, commit: &mut store::ProbeAnalysisCommit) {
    let references = std::mem::take(&mut commit.references);
    let previous_observation = commit.observations.remove(0);
    assert!(
        store
            .complete_probe_analysis_checked(commit)
            .unwrap()
            .is_none(),
        "a followup must retain its exact observed source guard"
    );
    commit.references = references;
    commit.observations.insert(0, previous_observation);
}

#[test]
fn model_followup_and_trigger_terminal_roll_back_together_with_unchanged_references() {
    let f = Fixture::new();
    let previous = f.store.prepare_probe_analysis(&f.captured(1)).unwrap();
    f.store.advance_probe_unit_to_analysis(&previous).unwrap();
    f.store.put_probe_detection_outcome(&previous.dispatch().job_id, &previous.dispatch().ledger_id,
        r#"{"status":"verified","available_reference_key":"same","model_reference_key":["old"]}"#).unwrap();
    f.store
        .finish_probe_unit(
            &previous.dispatch().job_id,
            &previous.dispatch().ledger_id,
            true,
            None,
        )
        .unwrap();
    let trigger = f.store.prepare_probe_analysis(&f.captured(2)).unwrap();
    f.store.advance_probe_unit_to_analysis(&trigger).unwrap();
    let claim = f
        .store
        .claim_probe_analysis("model-change", std::slice::from_ref(&trigger))
        .unwrap()
        .unwrap();
    let mut commit = store::ProbeAnalysisCommit {
        claim,
        outputs: vec![store::ProbeAnalysisOutput {
            dispatch: trigger.dispatch().clone(),
            detection_outcome_json:
                r#"{"status":"verified","available_reference_key":"same","model_reference_key":[]}"#
                    .into(),
            replacement: None,
        }],
        references: vec![previous.clone()],
        observations: vec![observation(&previous), observation(&trigger)],
        models: vec![
            store::StoredFingerprintModelSet::new(f.media, 1, "policy".into(), vec![]).unwrap(),
        ],
        followups: vec![
            store::ProbeAnalysisFollowups::new(
                trigger.clone(),
                vec![previous.clone()],
                "same".into(),
            )
            .unwrap(),
        ],
        refresh_job_id: None,
        refresh_error: None,
    };
    assert_unobserved_followup_rejected(&f.store, &mut commit);
    let db = rusqlite::Connection::open(f.dir.path().join("library.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_analysis_terminal BEFORE UPDATE OF status ON probe_job_units
        WHEN NEW.status='succeeded' BEGIN SELECT RAISE(ABORT,'injected terminal failure'); END;",
    )
    .unwrap();
    assert!(f.store.complete_probe_analysis_checked(&commit).is_err());
    assert!(
        f.store
            .active_probe_unit_for_ledger(&previous.dispatch().ledger_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.store.probe_job_units(&trigger.dispatch().job_id).unwrap()[0].status,
        "running"
    );
    db.execute_batch("DROP TRIGGER fail_analysis_terminal")
        .unwrap();
    assert!(
        f.store
            .complete_probe_analysis_checked(&commit)
            .unwrap()
            .is_some()
    );
    let successor = f
        .store
        .active_probe_unit_for_ledger(&previous.dispatch().ledger_id)
        .unwrap()
        .expect(
            "model changes must revisit prior verified files even when reference IDs stay the same",
        );
    assert_ne!(successor.job_id, previous.dispatch().job_id);
    assert_ne!(successor.job_id, trigger.dispatch().job_id);
    assert_eq!(successor.stage, ProbeDispatchStage::Analysis);
    assert_eq!(
        f.store
            .probe_job_units(&trigger.dispatch().job_id)
            .unwrap()
            .len(),
        1
    );
    assert!(
        f.store
            .complete_probe_analysis_checked(&commit)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.store
            .active_probe_unit_for_ledger(&previous.dispatch().ledger_id)
            .unwrap()
            .unwrap()
            .job_id,
        successor.job_id
    );
    assert_eq!(
        f.store
            .probe_job_units(&previous.dispatch().job_id)
            .unwrap()[0]
            .status,
        "succeeded"
    );
}
