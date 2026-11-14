use super::*;

#[test]
fn intro_only_observation_cannot_claim_a_later_complete_target_handoff() {
    let f = Fixture::new();
    let mut request = f.captured(1);
    request.sampling_mode = "adaptive".into();
    let head = model_bundle::capture(&f.store, &request.target.file, 1);
    let observed_head = f
        .store
        .prepare_probe_analysis(&request)
        .unwrap()
        .select_samples(std::slice::from_ref(&head.sample_id))
        .unwrap();
    let mut tail = head.clone();
    tail.sample_id = "later-tail".into();
    tail.kind = "outro".into();
    tail.window_start_ms = 1_020_000;
    tail.window_end_ms = 1_200_000;
    model_bundle::save(&f.store, &request.target.file, &tail);
    let tail_stage = ProbeStageKey {
        ledger_id: request.dispatch.ledger_id.clone(),
        context_key: request.capture_context_key.clone(),
        stage: ProbeStage::Outro,
    };
    f.store
        .claim_probe_stage(&tail_stage, &request.dispatch.job_id, 2)
        .unwrap();
    f.store
        .finish_probe_stage(
            &tail_stage,
            &request.dispatch.job_id,
            &ProbeStageCompletion::Succeeded,
            3,
        )
        .unwrap();
    request.required_kinds.push("outro".into());
    let ids = vec![head.sample_id.clone(), tail.sample_id.clone()];
    let complete = f
        .store
        .prepare_probe_analysis(&request)
        .unwrap()
        .select_samples(&ids)
        .unwrap();
    assert!(f.store.advance_probe_unit_to_analysis(&complete).unwrap());
    let pending = f
        .store
        .probe_analysis_participants(f.media, 1, None, 16)
        .unwrap();
    let stale = pending
        .iter()
        .filter(|p| p.observed_by(&observed_head, &[&head.sample_id]))
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        f.store
            .claim_probe_analysis("stale-intro-only", &stale)
            .unwrap()
            .is_none(),
        "new tail handoff must remain pending for a complete observed input"
    );
    assert_eq!(
        f.store.probe_job_units(&request.dispatch.job_id).unwrap()[0].status,
        "queued"
    );
    let observed_complete = f
        .store
        .prepare_probe_analysis(&request)
        .unwrap()
        .select_samples(&ids)
        .unwrap();
    let current = pending
        .into_iter()
        .filter(|p| p.observed_by(&observed_complete, &[&head.sample_id, &tail.sample_id]))
        .collect::<Vec<_>>();
    assert!(
        f.store
            .claim_probe_analysis("current-complete", &current)
            .unwrap()
            .is_some()
    );
}
#[path = "../support/model_bundle.rs"]
#[allow(dead_code)]
mod model_bundle;

#[test]
fn selected_reference_identity_excludes_unused_alternate_samples() {
    let f = Fixture::new();
    let mut request = f.captured(1);
    request.sampling_mode = "adaptive".into();
    let sample = model_bundle::capture(&f.store, &request.target.file, 1);
    let selected = [sample.sample_id.clone()];
    let before = f
        .store
        .prepare_probe_analysis(&request)
        .unwrap()
        .select_samples(&selected)
        .unwrap();
    assert!(f.store.advance_probe_unit_to_analysis(&before).unwrap());
    let mut alternate = sample.clone();
    alternate.sample_id = "alternate-unused".into();
    alternate.window_end_ms = 100_000;
    alternate.pcm_duration_ms = Some(100_000);
    model_bundle::save(&f.store, &request.target.file, &alternate);
    let same = f
        .store
        .prepare_probe_analysis(&request)
        .unwrap()
        .select_samples(&selected)
        .unwrap();
    assert_eq!(before.context(), same.context());
    let pending = f
        .store
        .probe_analysis_participants(f.media, 1, None, 16)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].context(),
        before.context(),
        "durable handoff must retain selected sample IDs"
    );
    let mut changed = sample;
    changed.fingerprint[0] += 1;
    model_bundle::save(&f.store, &request.target.file, &changed);
    let after = f
        .store
        .prepare_probe_analysis(&request)
        .unwrap()
        .select_samples(&selected)
        .unwrap();
    assert_ne!(before.context().context_key, after.context().context_key);
    assert!(
        f.store
            .prepare_probe_analysis(&request)
            .unwrap()
            .select_samples(&["absent".into()])
            .is_err()
    );
    drop(f.store);
    let reopened = Store::open(f.dir.path()).unwrap();
    let pending = reopened
        .probe_analysis_participants(f.media, 1, None, 16)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].context(),
        after.context(),
        "restart must refresh the pinned payload without adding alternate samples"
    );
}

#[test]
fn analysis_horizon_work_is_bounded_by_distinct_keys_not_season_backlog() {
    let f = Fixture::new();
    let first = f.store.prepare_probe_analysis(&f.captured(1)).unwrap();
    assert!(f.store.advance_probe_unit_to_analysis(&first).unwrap());
    let (small, before) = f.store.with_stats(store::stats::StatsScope::Library, || {
        f.store.pending_probe_analysis_keys(None, 16).unwrap()
    });
    for episode in 2..=128 {
        let participant = f
            .store
            .prepare_probe_analysis(&f.captured(episode))
            .unwrap();
        assert!(
            f.store
                .advance_probe_unit_to_analysis(&participant)
                .unwrap()
        );
    }
    let (large, after) = f.store.with_stats(store::stats::StatsScope::Library, || {
        f.store.pending_probe_analysis_keys(None, 16).unwrap()
    });
    assert_eq!(small, large);
    assert_eq!(large, vec![(f.media, 1)]);
    assert!(
        before.vm_steps > 0,
        "measure the real public SQLite operation"
    );
    assert!(
        after.vm_steps <= before.vm_steps + 100,
        "same-season backlog must not be scanned: {before:?} -> {after:?}"
    );
}

#[test]
fn cancellation_keeps_the_earliest_unfinished_reconciliation_obligation() {
    for same_reference in [true, false] {
        let f = Fixture::new();
        let first = f.store.prepare_probe_analysis(&f.captured(1)).unwrap();
        let second = f.store.prepare_probe_analysis(&f.captured(2)).unwrap();
        for participant in [&first, &second] {
            assert!(f.store.advance_probe_unit_to_analysis(participant).unwrap());
        }
        let later = serde_json::to_string(&store::ProbeAnalysisCursor(
            16,
            "library".into(),
            "file".into(),
        ))
        .unwrap();
        let earlier = serde_json::to_string(&store::ProbeAnalysisCursor(
            3,
            "library".into(),
            "file".into(),
        ))
        .unwrap();
        for (participant, reference, cursor) in [
            (&first, "old", later),
            (
                &second,
                if same_reference { "old" } else { "new" },
                earlier.clone(),
            ),
        ] {
            assert!(
                f.store
                    .refresh_probe_analysis_targets(&store::ProbeAnalysisReconcile {
                        trigger: participant.clone(),
                        expected_cursor: None,
                        reference_key: reference.into(),
                        candidates: vec![],
                        next_cursor: Some(cursor),
                    })
                    .unwrap()
            );
        }
        f.store
            .cancel_probe_unit(
                &first.dispatch().job_id,
                &first.dispatch().ledger_id,
                "removed",
            )
            .unwrap();
        let state = f
            .store
            .probe_analysis_refresh_state(second.dispatch())
            .unwrap()
            .unwrap();
        assert!(!state.reconcile_done);
        assert_eq!(
            state.reconcile_after,
            if same_reference { Some(earlier) } else { None }
        );
        assert_eq!(
            state.wanted_reference_key,
            if same_reference { "old" } else { "new" }
        );
    }
}
