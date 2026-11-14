use domain::{MediaId, MediaKind};
#[path = "probe_analysis_dispatch/quantized.rs"]
mod quantized;
use store::{
    ProbeAnalysisRequest, ProbeDispatchStage, ProbeJobUnitSpec, ProbeStage, ProbeStageCompletion,
    ProbeStageKey, Store,
};
#[path = "probe_analysis_dispatch/cancellation.rs"]
mod cancellation;
#[path = "probe_analysis_dispatch/model_followups.rs"]
mod model_followups;
#[path = "probe_analysis_dispatch/reconciliation.rs"]
mod reconciliation;

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    media: MediaId,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let media = MediaId::new();
        store
            .insert_media(&domain::Media {
                id: media,
                kind: MediaKind::Tv,
                title: "Analysis".into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        Self { dir, store, media }
    }

    fn captured(&self, episode: u32) -> ProbeAnalysisRequest {
        let library = self.store.default_library(MediaKind::Tv).unwrap().unwrap();
        self.captured_in(episode, &library)
    }

    fn captured_in(&self, episode: u32, library: &store::Library) -> ProbeAnalysisRequest {
        self.store
            .set_library_intro_settings(&library.id, true, true)
            .unwrap();
        let file = store::test_support::publish_fixture_file(
            &self.store,
            &store::test_support::FixtureFile {
                media_id: self.media,
                path: library.root_paths[0]
                    .join(format!("S01E{episode}.mkv"))
                    .display()
                    .to_string(),
                season: Some(1),
                episode: Some(episode),
                ..Default::default()
            },
        )
        .unwrap();
        let ledger = file.file_id.to_string();
        let job = format!("job-{episode:03}");
        assert!(
            self.store
                .create_probe_job(
                    &job,
                    "fingerprint_probe",
                    &self.media.to_string(),
                    Some(1),
                    &format!("ledger:{ledger}"),
                    &[ProbeJobUnitSpec {
                        ledger_id: &ledger,
                        kind: "tv",
                        force_fingerprint: true,
                        reuse_fingerprint_cache: true,
                        overwrite_markers: false,
                        reuse_media_info_cache: true,
                    }]
                )
                .unwrap()
        );
        assert!(self.store.start_probe_unit(&job, &ledger).unwrap());
        let cache = store::FingerprintCacheEntry {
            cache_key: format!("capture-{episode}"),
            algorithm_version: 1,
            sample_duration_secs: 180,
            media_duration_ms: Some(1_800_000),
            intro: marker::CapturedFingerprint {
                window: marker::SampleWindow::new(0, 180_000),
                words: vec![1, 2, episode],
                pcm_duration_ms: Some(180_000),
                metrics: Default::default(),
            },
            outro: None,
        };
        self.store
            .put_fingerprint_cache_checked(&ledger, Some(&file.version), &cache)
            .unwrap();
        let stage = ProbeStageKey {
            ledger_id: ledger.clone(),
            context_key: cache.cache_key,
            stage: ProbeStage::Intro,
        };
        self.store.claim_probe_stage(&stage, &job, 0).unwrap();
        self.store
            .finish_probe_stage(&stage, &job, &ProbeStageCompletion::Succeeded, 1)
            .unwrap();
        ProbeAnalysisRequest {
            dispatch: store::ProbeDispatchId {
                job_id: job,
                ledger_id: ledger,
                stage: ProbeDispatchStage::Fingerprint,
            },
            target: self
                .store
                .file_coverage_by_id(file.file_id)
                .unwrap()
                .unwrap(),
            season: 1,
            sampling_mode: "full_window".into(),
            analysis_policy_key: "policy".into(),
            analysis_policy_json: serde_json::to_string(&marker::SamplingPolicy::default())
                .unwrap(),
            capture_context_key: stage.context_key,
            required_kinds: vec!["intro".into()],
        }
    }
}

#[test]
fn handoff_survives_reopen_without_completing_capture_job() {
    let f = Fixture::new();
    let request = f.captured(1);
    let prepared = f.store.prepare_probe_analysis(&request).unwrap();
    assert!(f.store.advance_probe_unit_to_analysis(&prepared).unwrap());
    assert!(!f.store.advance_probe_unit_to_analysis(&prepared).unwrap());
    let job = f
        .store
        .get_probe_job(&request.dispatch.job_id)
        .unwrap()
        .unwrap();
    assert!(job.is_active());
    assert_eq!((job.completed, job.succeeded), (0, 0));
    drop(f.store);
    let reopened = Store::open(f.dir.path()).unwrap();
    let keys = reopened.pending_probe_analysis_keys(None, 16).unwrap();
    assert_eq!(keys, vec![(f.media, 1)]);
    let participants = reopened
        .probe_analysis_participants(f.media, 1, None, 16)
        .unwrap();
    assert_eq!(participants.len(), 1);
    assert_eq!(participants[0].context(), prepared.context());
    assert!(
        reopened
            .pending_probe_dispatch_ids(ProbeDispatchStage::Fingerprint, 16)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn interrupted_claim_is_requeued_with_new_token_and_same_context() {
    let f = Fixture::new();
    let prepared = f.store.prepare_probe_analysis(&f.captured(1)).unwrap();
    f.store.advance_probe_unit_to_analysis(&prepared).unwrap();
    let participants = f
        .store
        .probe_analysis_participants(f.media, 1, None, 16)
        .unwrap();
    let old = f
        .store
        .claim_probe_analysis("old", &participants)
        .unwrap()
        .unwrap();
    assert!(
        f.store
            .claim_probe_analysis("duplicate", &participants)
            .unwrap()
            .is_none()
    );
    f.store.reset_running_probe_units().unwrap();
    let resumed = f
        .store
        .probe_analysis_participants(f.media, 1, None, 16)
        .unwrap();
    let current = f
        .store
        .claim_probe_analysis("current", &resumed)
        .unwrap()
        .unwrap();
    f.store.requeue_probe_analysis(&old).unwrap();
    assert!(
        f.store
            .probe_analysis_participants(f.media, 1, None, 16)
            .unwrap()
            .is_empty()
    );
    f.store.requeue_probe_analysis(&current).unwrap();
    assert_eq!(
        f.store
            .probe_analysis_participants(f.media, 1, None, 16)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn analysis_handoff_rejects_same_length_capture_replacement() {
    let f = Fixture::new();
    let request = f.captured(1);
    let prepared = f.store.prepare_probe_analysis(&request).unwrap();
    let mut cache = f
        .store
        .get_fingerprint_cache(&request.dispatch.ledger_id)
        .unwrap()
        .unwrap();
    cache.intro.words[1] = 99;
    f.store
        .put_fingerprint_cache(&request.dispatch.ledger_id, &cache)
        .unwrap();
    assert!(!f.store.advance_probe_unit_to_analysis(&prepared).unwrap());
    assert_eq!(
        f.store.pending_probe_analysis_keys(None, 16).unwrap().len(),
        0
    );
}

#[test]
fn changed_queued_evidence_is_reprepared_without_poisoning_other_participants() {
    let f = Fixture::new();
    let first = f.store.prepare_probe_analysis(&f.captured(1)).unwrap();
    let second = f.store.prepare_probe_analysis(&f.captured(2)).unwrap();
    for item in [&first, &second] {
        assert!(f.store.advance_probe_unit_to_analysis(item).unwrap());
    }
    let mut cache = f
        .store
        .get_fingerprint_cache(&first.dispatch().ledger_id)
        .unwrap()
        .unwrap();
    cache.intro.words[0] = 333;
    f.store
        .put_fingerprint_cache(&first.dispatch().ledger_id, &cache)
        .unwrap();
    let current = f
        .store
        .probe_analysis_participants(f.media, 1, None, 16)
        .unwrap();
    assert_eq!(current.len(), 2);
    assert_ne!(
        current
            .iter()
            .find(|p| p.dispatch().ledger_id == first.dispatch().ledger_id)
            .unwrap()
            .context()
            .context_key,
        first.context().context_key
    );
    assert!(
        f.store
            .claim_probe_analysis("new-evidence", &current)
            .unwrap()
            .is_some()
    );
}

#[test]
fn changed_references_during_claim_cannot_be_lost_at_completion() {
    let f = Fixture::new();
    let first = f.store.prepare_probe_analysis(&f.captured(1)).unwrap();
    f.store.advance_probe_unit_to_analysis(&first).unwrap();
    let rows = f
        .store
        .probe_analysis_participants(f.media, 1, None, 16)
        .unwrap();
    let claim = f.store.claim_probe_analysis("old", &rows).unwrap().unwrap();
    let later = f.store.prepare_probe_analysis(&f.captured(2)).unwrap();
    f.store.advance_probe_unit_to_analysis(&later).unwrap();
    assert!(
        f.store
            .refresh_probe_analysis_targets(&store::ProbeAnalysisReconcile {
                trigger: later,
                expected_cursor: None,
                reference_key: "new".into(),
                candidates: vec![first.clone()],
                next_cursor: None,
            })
            .unwrap()
    );
    let commit = store::ProbeAnalysisCommit {
        claim: claim.clone(),
        outputs: vec![store::ProbeAnalysisOutput {
            dispatch: rows[0].dispatch().clone(),
            detection_outcome_json: "{\"available_reference_key\":\"old\"}".into(),
            replacement: None,
        }],
        references: vec![],
        observations: vec![store::SourceObservation {
            file_id: first.context().target.file.file_id,
            expected: first.context().target.file.version.clone(),
            presence: domain::FilePresence::Present,
        }],
        models: vec![],
        followups: vec![],
        refresh_job_id: None,
        refresh_error: None,
    };
    assert!(
        f.store
            .complete_probe_analysis_checked(&commit)
            .unwrap()
            .is_none()
    );
    f.store.requeue_probe_analysis(&claim).unwrap();
    assert_eq!(
        f.store
            .probe_analysis_participants(f.media, 1, None, 16)
            .unwrap()
            .len(),
        2
    );
    assert!(
        f.store
            .get_probe_job(&first.dispatch().job_id)
            .unwrap()
            .unwrap()
            .is_active()
    );
}

#[test]
fn twenty_four_captures_release_fingerprint_window_before_analysis_completes() {
    let f = Fixture::new();
    for episode in 1..=24 {
        let prepared = f
            .store
            .prepare_probe_analysis(&f.captured(episode))
            .unwrap();
        assert!(f.store.advance_probe_unit_to_analysis(&prepared).unwrap());
    }
    assert!(
        f.store
            .pending_probe_dispatch_ids(ProbeDispatchStage::Fingerprint, 16)
            .unwrap()
            .is_empty()
    );
    let first = f
        .store
        .probe_analysis_participants(f.media, 1, None, 16)
        .unwrap();
    assert_eq!(first.len(), 16);
    let last = first.last().unwrap().dispatch();
    let rest = f
        .store
        .probe_analysis_participants(f.media, 1, Some((&last.job_id, &last.ledger_id)), 16)
        .unwrap();
    assert_eq!(rest.len(), 8);
    assert!(first.iter().chain(&rest).all(|item| {
        f.store
            .get_probe_job(&item.dispatch().job_id)
            .unwrap()
            .unwrap()
            .completed
            == 0
    }));
}

#[test]
fn result_and_exact_claim_completion_share_one_commit() {
    let f = Fixture::new();
    let request = f.captured(1);
    let prepared = f.store.prepare_probe_analysis(&request).unwrap();
    f.store.advance_probe_unit_to_analysis(&prepared).unwrap();
    let participants = f
        .store
        .probe_analysis_participants(f.media, 1, None, 16)
        .unwrap();
    let claim = f
        .store
        .claim_probe_analysis("token", &participants)
        .unwrap()
        .unwrap();
    let output = store::ProbeAnalysisOutput {
        dispatch: participants[0].dispatch().clone(),
        detection_outcome_json: "{\"status\":\"uncertain\"}".into(),
        replacement: None,
    };
    let commit = store::ProbeAnalysisCommit {
        claim: claim.clone(),
        outputs: vec![output],
        references: participants.clone(),
        observations: vec![store::SourceObservation {
            file_id: request.target.file.file_id,
            expected: request.target.file.version.clone(),
            presence: domain::FilePresence::Present,
        }],
        models: vec![],
        followups: vec![],
        refresh_job_id: None,
        refresh_error: None,
    };
    assert!(
        f.store
            .complete_probe_analysis_checked(&commit)
            .unwrap()
            .is_some()
    );
    assert!(
        f.store
            .complete_probe_analysis_checked(&commit)
            .unwrap()
            .is_none()
    );
    let job = f
        .store
        .get_probe_job(&request.dispatch.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        (job.status.as_str(), job.completed, job.succeeded),
        ("succeeded", 1, 1)
    );
    assert_eq!(
        f.store.probe_job_units(&job.id).unwrap()[0]
            .detection_outcome_json
            .as_deref(),
        Some("{\"status\":\"uncertain\"}")
    );
}

#[test]
fn later_capture_cannot_be_completed_by_an_earlier_claim() {
    let f = Fixture::new();
    let first = f.store.prepare_probe_analysis(&f.captured(1)).unwrap();
    f.store.advance_probe_unit_to_analysis(&first).unwrap();
    let participants = f
        .store
        .probe_analysis_participants(f.media, 1, None, 16)
        .unwrap();
    let claim = f
        .store
        .claim_probe_analysis("first", &participants)
        .unwrap()
        .unwrap();
    let later = f.store.prepare_probe_analysis(&f.captured(2)).unwrap();
    f.store.advance_probe_unit_to_analysis(&later).unwrap();
    let output = store::ProbeAnalysisOutput {
        dispatch: later.dispatch().clone(),
        detection_outcome_json: "{}".into(),
        replacement: None,
    };
    let commit = store::ProbeAnalysisCommit {
        claim,
        outputs: vec![output],
        references: participants,
        observations: vec![],
        models: vec![],
        followups: vec![],
        refresh_job_id: None,
        refresh_error: None,
    };
    assert!(
        f.store
            .complete_probe_analysis_checked(&commit)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.store
            .get_probe_job(&later.dispatch().job_id)
            .unwrap()
            .unwrap()
            .completed,
        0
    );
}

#[test]
fn later_reference_creates_own_durable_successor_for_terminal_uncertainty() {
    let f = Fixture::new();
    let first = f.store.prepare_probe_analysis(&f.captured(1)).unwrap();
    f.store.advance_probe_unit_to_analysis(&first).unwrap();
    let rows = f
        .store
        .probe_analysis_participants(f.media, 1, None, 16)
        .unwrap();
    let claim = f
        .store
        .claim_probe_analysis("first", &rows)
        .unwrap()
        .unwrap();
    let commit = store::ProbeAnalysisCommit {
        claim,
        outputs: vec![store::ProbeAnalysisOutput {
            dispatch: rows[0].dispatch().clone(),
            detection_outcome_json:
                "{\"status\":\"uncertain\",\"available_reference_key\":\"one\"}".into(),
            replacement: None,
        }],
        references: vec![],
        observations: vec![store::SourceObservation {
            file_id: first.context().target.file.file_id,
            expected: first.context().target.file.version.clone(),
            presence: domain::FilePresence::Present,
        }],
        models: vec![],
        followups: vec![],
        refresh_job_id: None,
        refresh_error: None,
    };
    assert!(
        f.store
            .complete_probe_analysis_checked(&commit)
            .unwrap()
            .is_some()
    );
    let later = f.store.prepare_probe_analysis(&f.captured(2)).unwrap();
    f.store.advance_probe_unit_to_analysis(&later).unwrap();
    let page = store::ProbeAnalysisReconcile {
        trigger: later.clone(),
        expected_cursor: None,
        reference_key: "two".into(),
        candidates: vec![first.clone()],
        next_cursor: None,
    };
    assert!(f.store.refresh_probe_analysis_targets(&page).unwrap());
    assert!(!f.store.refresh_probe_analysis_targets(&page).unwrap());
    let successor = f
        .store
        .active_probe_unit_for_ledger(&first.dispatch().ledger_id)
        .unwrap()
        .unwrap();
    assert_ne!(successor.job_id, first.dispatch().job_id);
    assert_ne!(successor.job_id, later.dispatch().job_id);
    assert_eq!(successor.stage, ProbeDispatchStage::Analysis);
    assert_eq!(
        f.store
            .get_probe_job(&first.dispatch().job_id)
            .unwrap()
            .unwrap()
            .status,
        "succeeded"
    );
    drop(f.store);
    let reopened = Store::open(f.dir.path()).unwrap();
    assert_eq!(
        reopened
            .probe_analysis_participants(f.media, 1, None, 16)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn refresh_capture_failure_finishes_ready_analysis_without_publishing_partial_batch() {
    let f = Fixture::new();
    let mut requests = (1..=2)
        .map(|episode| f.captured(episode))
        .collect::<Vec<_>>();
    for request in &requests {
        f.store
            .cancel_probe_job(&request.dispatch.job_id, "replace fixture job")
            .unwrap();
    }
    let specs = requests
        .iter()
        .map(|request| ProbeJobUnitSpec {
            ledger_id: &request.dispatch.ledger_id,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: true,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        })
        .collect::<Vec<_>>();
    assert!(
        f.store
            .create_probe_job(
                "refresh",
                "marker_refresh",
                &f.media.to_string(),
                None,
                "refresh",
                &specs
            )
            .unwrap()
    );
    for request in &mut requests {
        request.dispatch.job_id = "refresh".into();
        f.store
            .start_probe_unit("refresh", &request.dispatch.ledger_id)
            .unwrap();
        assert!(
            f.store
                .advance_probe_unit_to_fingerprint("refresh", &request.dispatch.ledger_id)
                .unwrap()
        );
        assert!(
            f.store
                .start_probe_unit("refresh", &request.dispatch.ledger_id)
                .unwrap()
        );
    }
    let first = f.store.prepare_probe_analysis(&requests[0]).unwrap();
    assert!(f.store.advance_probe_unit_to_analysis(&first).unwrap());
    f.store
        .finish_probe_unit_with_status(
            "refresh",
            &requests[1].dispatch.ledger_id,
            "partial",
            Some("timeout"),
            Some("tail unavailable"),
        )
        .unwrap();
    assert!(
        f.store
            .fail_probe_analysis_refresh_if_capture_failed("refresh")
            .unwrap()
            .is_some()
    );
    let job = f.store.get_probe_job("refresh").unwrap().unwrap();
    assert_eq!(job.status, "failed");
    assert_eq!(job.completed, 2);
    assert!(
        f.store
            .active_probe_unit_for_ledger(&requests[0].dispatch.ledger_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn refresh_survivor_finishes_successfully_after_other_participant_is_cancelled() {
    let f = Fixture::new();
    let mut requests = (1..=2)
        .map(|episode| f.captured(episode))
        .collect::<Vec<_>>();
    for request in &requests {
        f.store
            .cancel_probe_job(&request.dispatch.job_id, "replace fixture job")
            .unwrap();
    }
    let specs = requests
        .iter()
        .map(|request| ProbeJobUnitSpec {
            ledger_id: &request.dispatch.ledger_id,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: true,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        })
        .collect::<Vec<_>>();
    f.store
        .create_probe_job(
            "refresh",
            "marker_refresh",
            &f.media.to_string(),
            None,
            "refresh",
            &specs,
        )
        .unwrap();
    f.store
        .cancel_probe_unit(
            "refresh",
            &requests[0].dispatch.ledger_id,
            "Library removed",
        )
        .unwrap();
    let request = &mut requests[1];
    request.dispatch.job_id = "refresh".into();
    f.store
        .start_probe_unit("refresh", &request.dispatch.ledger_id)
        .unwrap();
    f.store
        .advance_probe_unit_to_fingerprint("refresh", &request.dispatch.ledger_id)
        .unwrap();
    f.store
        .start_probe_unit("refresh", &request.dispatch.ledger_id)
        .unwrap();
    let participant = f.store.prepare_probe_analysis(request).unwrap();
    assert!(
        f.store
            .advance_probe_unit_to_analysis(&participant)
            .unwrap()
    );
    let participants = f
        .store
        .probe_analysis_job_participants("refresh")
        .unwrap()
        .unwrap();
    let claim = f
        .store
        .claim_probe_analysis("survivor", &participants)
        .unwrap()
        .unwrap();
    let commit = store::ProbeAnalysisCommit {
        claim,
        outputs: vec![store::ProbeAnalysisOutput {
            dispatch: participants[0].dispatch().clone(),
            detection_outcome_json: "{}".into(),
            replacement: None,
        }],
        references: vec![],
        observations: vec![store::SourceObservation {
            file_id: request.target.file.file_id,
            expected: request.target.file.version.clone(),
            presence: domain::FilePresence::Present,
        }],
        models: vec![],
        followups: vec![],
        refresh_job_id: Some("refresh".into()),
        refresh_error: None,
    };
    assert!(
        f.store
            .complete_probe_analysis_checked(&commit)
            .unwrap()
            .is_some()
    );
    let job = f.store.get_probe_job("refresh").unwrap().unwrap();
    assert_eq!(
        (job.status.as_str(), job.completed, job.succeeded),
        ("succeeded", 2, 1)
    );
}
