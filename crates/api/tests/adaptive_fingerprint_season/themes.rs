use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

struct Themes {
    forbid_seed_reads: AtomicBool,
}
impl marker::FingerprintCaptureEngine for Themes {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        let name = request.path.file_stem().unwrap().to_str().unwrap();
        let episode = name.rsplit('E').next().unwrap().parse::<u32>().unwrap();
        assert!(
            !self.forbid_seed_reads.load(Ordering::SeqCst) || episode > 3,
            "unchanged seed cannot be recaptured"
        );
        let mut capture =
            marker::FingerprintCaptureEngine::capture_window(&SharedMatchEngine, request)?;
        capture.words[1] = episode;
        Ok(capture)
    }
}
impl marker::FingerprintEngine for Themes {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(
        &self,
        a: &[u32],
        b: &[u32],
        min: f32,
        max: f32,
    ) -> Option<marker::CommonSegment> {
        self.find_common_segments(a, b, min, max).into_iter().next()
    }
    fn find_common_segments(
        &self,
        a: &[u32],
        b: &[u32],
        _: f32,
        _: f32,
    ) -> Vec<marker::CommonSegment> {
        let interval = |start: f32, duration: f32| marker::CommonSegment {
            start1_sec: start - a[0] as f32 / 1000.0,
            end1_sec: start + duration - a[0] as f32 / 1000.0,
            start2_sec: start - b[0] as f32 / 1000.0,
            end2_sec: start + duration - b[0] as f32 / 1000.0,
            duration_sec: duration,
            score: 0.95,
        };
        if a[0] > 900_000 {
            return vec![interval(1630.0, 80.0)];
        }
        let mut result = Vec::new();
        if a[1] <= 3 && b[1] <= 3 {
            result.push(interval(10.0, 80.0));
        }
        if [1, 4, 5].contains(&a[1]) && [1, 4, 5].contains(&b[1]) {
            result.push(interval(110.0, 50.0));
        }
        result
    }
}

fn outcome(store: &Store, row: &FileCoverage) -> (String, serde_json::Value) {
    let job = store
        .latest_probe_job_for_ledger(&row.file.file_id.to_string())
        .unwrap()
        .unwrap();
    let units = store.probe_job_units(&job).unwrap();
    let unit = units
        .iter()
        .find(|unit| unit.ledger_id == row.file.file_id.to_string())
        .unwrap();
    assert_eq!(unit.status, "succeeded");
    (
        job,
        serde_json::from_str(unit.detection_outcome_json.as_deref().unwrap()).unwrap(),
    )
}

fn reference_ids(store: &Store, media: MediaId) -> std::collections::BTreeSet<String> {
    store
        .list_fingerprint_models(&media.to_string(), 1)
        .unwrap()
        .into_iter()
        .flat_map(|model| {
            let template: marker::adaptive::TemplateModel =
                serde_json::from_str(&model.model_json).unwrap();
            template
                .references
                .into_iter()
                .map(|reference| reference.sample_id)
        })
        .collect()
}

#[tokio::test]
async fn competing_theme_rebuild_revisits_verified_files_with_their_own_jobs() {
    let mut outcomes = Vec::new();
    for incremental in [true, false] {
        let (_temp, store, previous, clock, media, root) = setup();
        previous.shutdown_workers().await.unwrap();
        let engine = Arc::new(Themes {
            forbid_seed_reads: AtomicBool::new(false),
        });
        let manager = Arc::new(
            ProbeManager::with_engines(store.clone(), engine.clone(), engine.clone()).unwrap(),
        );
        manager.set_clock(clock.clone());
        manager.try_start_workers();
        let mut rows = Vec::new();
        let mut old = None;
        let mut old_references = std::collections::BTreeSet::new();
        for (first, last) in if incremental {
            vec![(1, 3), (4, 5)]
        } else {
            vec![(1, 5)]
        } {
            for episode in first..=last {
                let row = row(&store, media, &root, 1, episode);
                manager
                    .enqueue_direct_fingerprint(
                        unit(row.clone()),
                        Some(1_800_000),
                        row.file.version.source_version.clone(),
                        180,
                    )
                    .unwrap();
                rows.push(row);
            }
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while rows.iter().any(|row| {
                    store
                        .active_probe_unit_for_ledger(&row.file.file_id.to_string())
                        .unwrap()
                        .is_some_and(|unit| unit.stage != store::ProbeDispatchStage::Analysis)
                }) {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            drain(&store, &manager, &clock, &rows).await;
            if incremental && rows.len() == 3 {
                let initial = outcome(&store, &rows[0]);
                assert_eq!(initial.1["status"], "verified");
                old = Some((
                    initial,
                    store
                        .get_file_marker(rows[0].file.file_id, rows[0].units[0])
                        .unwrap(),
                ));
                old_references = reference_ids(&store, media);
                assert!(!old_references.is_empty());
                engine.forbid_seed_reads.store(true, Ordering::SeqCst);
            }
        }
        let final_result = outcome(&store, &rows[0]);
        assert_eq!(
            final_result.1["status"], "partial",
            "old Intro must be reviewed while independent Outro stays verified"
        );
        assert_eq!(final_result.1["kinds"]["intro"]["status"], "uncertain");
        assert_eq!(final_result.1["kinds"]["outro"]["status"], "verified");
        if let Some((before, marker)) = old {
            assert!(
                old_references.is_subset(&reference_ids(&store, media)),
                "prior reference identities remain present despite the competing theme"
            );
            assert_ne!(
                final_result.0, before.0,
                "prior file requires its own successor"
            );
            assert_ne!(
                final_result.1["model_reference_key"],
                before.1["model_reference_key"]
            );
            let after = store
                .get_file_marker(rows[0].file.file_id, rows[0].units[0])
                .unwrap()
                .unwrap();
            let marker = marker.unwrap();
            assert_eq!(
                (after.intro_start_ms, after.intro_end_ms),
                (marker.intro_start_ms, marker.intro_end_ms)
            );
            assert_eq!(
                (after.outro_start_ms, after.outro_end_ms),
                (marker.outro_start_ms, marker.outro_end_ms)
            );
        }
        outcomes.push(
            rows.iter()
                .map(|row| outcome(&store, row).1["status"].clone())
                .collect::<Vec<_>>(),
        );
        manager.shutdown_workers().await.unwrap();
    }
    assert_eq!(
        outcomes[0], outcomes[1],
        "incremental and batch must converge on typed decisions"
    );
}
