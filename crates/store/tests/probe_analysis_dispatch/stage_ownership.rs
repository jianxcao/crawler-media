use super::*;

#[test]
fn a_stale_capture_start_cannot_claim_queued_analysis() {
    let f = Fixture::new();
    let request = f.captured(1);
    let prepared = f.store.prepare_probe_analysis(&request).unwrap();
    assert!(f.store.advance_probe_unit_to_analysis(&prepared).unwrap());

    for stage in [
        ProbeDispatchStage::Metadata,
        ProbeDispatchStage::Fingerprint,
        ProbeDispatchStage::Analysis,
    ] {
        assert!(
            !f.store
                .start_probe_unit(&request.dispatch.job_id, &request.dispatch.ledger_id, stage)
                .unwrap(),
            "ordinary {stage:?} admission must leave Analysis available to its own consumer"
        );
    }
    let unit = f.store.probe_job_units(&request.dispatch.job_id).unwrap();
    assert_eq!(unit[0].stage, ProbeDispatchStage::Analysis);
    assert_eq!(unit[0].status, "queued");
    let participants = f
        .store
        .probe_analysis_participants(f.media, 1, None, 16)
        .unwrap();
    assert_eq!(participants.len(), 1);
    assert!(
        f.store
            .claim_probe_analysis("analysis-owner", &participants)
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_stale_metadata_start_cannot_claim_queued_capture() {
    let f = Fixture::new();
    let request = f.captured(1);
    f.store
        .cancel_probe_job(&request.dispatch.job_id, "replace fixture job")
        .unwrap();
    let file = request.dispatch.ledger_id;
    assert!(
        f.store
            .create_probe_job(
                "refresh",
                "marker_refresh",
                &f.media.to_string(),
                Some(1),
                "refresh",
                &[ProbeJobUnitSpec {
                    ledger_id: &file,
                    kind: "tv",
                    force_fingerprint: true,
                    reuse_fingerprint_cache: true,
                    overwrite_markers: true,
                    reuse_media_info_cache: true,
                }],
            )
            .unwrap()
    );
    assert!(
        f.store
            .start_probe_unit("refresh", &file, ProbeDispatchStage::Metadata)
            .unwrap()
    );
    assert!(
        f.store
            .advance_probe_unit_to_fingerprint("refresh", &file)
            .unwrap()
    );
    assert!(
        !f.store
            .start_probe_unit("refresh", &file, ProbeDispatchStage::Metadata)
            .unwrap()
    );
    let units = f.store.probe_job_units("refresh").unwrap();
    assert_eq!(units[0].stage, ProbeDispatchStage::Fingerprint);
    assert_eq!(units[0].status, "queued");
    assert!(
        f.store
            .start_probe_unit("refresh", &file, ProbeDispatchStage::Fingerprint)
            .unwrap()
    );
}
