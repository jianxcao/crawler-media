//! Disabling optional detection cancels its durable work while Library work remains usable.
#[path = "library_optional_probe_switches/support.rs"]
mod support;

use support::{Fixture, create_job, runtime};

fn disabling_optional_work(field: &str) {
    let fixture = Fixture::new();
    let ordinary = fixture.state.acquire_library_work(&fixture.target).unwrap();
    runtime().block_on(async {
        fixture.patch(field, false).await;
        {
            let handle = fixture.state.store();
            let store = handle.as_ref();
            for job in &fixture.jobs {
                let expected = if job.library == fixture.target && job.kind != "media_probe" {
                    "cancelled"
                } else {
                    job.initial
                };
                let actual = store.get_probe_job(&job.id).unwrap().unwrap();
                assert_eq!(
                    actual.status, expected,
                    "{} {} in {}",
                    job.kind, job.initial, job.library
                );
                let units = store.probe_job_units(&job.id).unwrap();
                assert_eq!(units.len(), 1);
                assert_eq!(units[0].status, expected);
            }
        }
        assert!(
            !ordinary.control().is_cancelled(),
            "basic metadata/scan work must survive the toggle"
        );
        let fresh = fixture.state.acquire_library_work(&fixture.target).unwrap();
        drop(fresh);
        fixture.patch(field, true).await;
        let file = &fixture
            .jobs
            .iter()
            .find(|job| job.library == fixture.target && job.kind == "fingerprint_probe")
            .unwrap()
            .file;
        let fresh_id = uuid::Uuid::new_v4().to_string();
        let handle = fixture.state.store();
        let store = handle.as_ref();
        create_job(store, &fresh_id, "fingerprint_probe", file);
        assert_eq!(
            store.get_probe_job(&fresh_id).unwrap().unwrap().status,
            "queued"
        );
        for old in fixture
            .jobs
            .iter()
            .filter(|job| job.library == fixture.target && job.kind != "media_probe")
        {
            assert_eq!(
                store.get_probe_job(&old.id).unwrap().unwrap().status,
                "cancelled",
                "re-enabling must not revive cancelled work"
            );
        }
    });
}

#[test]
fn disabling_intro_detection_cancels_only_target_optional_queued_and_running_work() {
    disabling_optional_work("detect_intros");
}

#[test]
fn disabling_fingerprint_cancels_only_target_optional_queued_and_running_work() {
    disabling_optional_work("enable_fingerprint");
}

fn metadata_completion_obeys_switch(field: &str) {
    let fixture = Fixture::new();
    runtime().block_on(async {
        fixture.patch(field, false).await;
        let parent = fixture
            .jobs
            .iter()
            .find(|job| {
                job.library == fixture.target
                    && job.kind == "media_probe"
                    && job.initial == "running"
            })
            .unwrap();
        let successor = uuid::Uuid::new_v4().to_string();
        let handle = fixture.state.store();
        let store = handle.as_ref();
        let queued = store
            .complete_media_probe_and_enqueue_fingerprint(
                &parent.id,
                &parent.file.file_id.to_string(),
                &successor,
            )
            .unwrap();
        assert!(
            !queued,
            "a metadata result arriving after the switch must not create optional work"
        );
        assert!(store.get_probe_job(&successor).unwrap().is_none());
        assert_eq!(
            store.get_probe_job(&parent.id).unwrap().unwrap().status,
            "succeeded",
            "mandatory media information still completes normally"
        );
    });
}

#[test]
fn metadata_completion_after_detection_disable_does_not_enqueue_fingerprint() {
    metadata_completion_obeys_switch("detect_intros");
}

#[test]
fn metadata_completion_after_fingerprint_disable_does_not_enqueue_fingerprint() {
    metadata_completion_obeys_switch("enable_fingerprint");
}

fn optional_admission_obeys_disabled_switches(kind: &str) {
    let mut results = Vec::new();
    for field in ["detect_intros", "enable_fingerprint"] {
        let fixture = Fixture::new();
        runtime().block_on(async {
            fixture.patch(field, false).await;
            let parent = fixture
                .jobs
                .iter()
                .find(|job| {
                    job.library == fixture.target
                        && job.kind == "media_probe"
                        && job.initial == "queued"
                })
                .unwrap();
            let handle = fixture.state.store();
            let store = handle.as_ref();
            let ledger = parent.file.file_id.to_string();
            // Release the mandatory unit's unique active-file slot first, so
            // optional admission cannot accidentally fail for a duplicate.
            let (completed, _, _) = store
                .finish_probe_unit(&parent.id, &ledger, true, None)
                .unwrap();
            assert_eq!(completed.status, "succeeded");
            let id = uuid::Uuid::new_v4().to_string();
            let created = store
                .create_probe_job(
                    &id,
                    kind,
                    &parent.file.media_id.to_string(),
                    Some(1),
                    &format!("{kind}:{id}"),
                    &[store::ProbeJobUnitSpec {
                        ledger_id: &ledger,
                        kind: "tv",
                        force_fingerprint: true,
                        reuse_fingerprint_cache: false,
                        overwrite_markers: kind == "marker_refresh",
                        reuse_media_info_cache: true,
                    }],
                )
                .unwrap();
            results.push((field, created, store.get_probe_job(&id).unwrap().is_some()));
        });
    }
    let expected = kind == "marker_refresh";
    assert_eq!(
        results,
        vec![
            ("detect_intros", expected, expected),
            ("enable_fingerprint", expected, expected),
        ],
        "only a new explicit manual refresh may override the automatic feature switches"
    );
}

#[test]
fn disabled_library_rejects_forced_fingerprint_job() {
    optional_admission_obeys_disabled_switches("fingerprint_probe");
}

#[test]
fn disabled_library_allows_new_explicit_manual_marker_refresh() {
    optional_admission_obeys_disabled_switches("marker_refresh");
}
