use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

async fn seed_adaptive_references(s: &ProbeScenario, mode: &str) {
    if mode == "adaptive" {
        assert_eq!(super::pair_cache::refresh(s).await.status, "succeeded");
        let target = &s.ledgers[0];
        s.store
            .delete_file_marker(target.file.file_id, target.units[0])
            .unwrap();
    }
}

fn seed_reference_markers(scenario: &ProbeScenario) -> Vec<store::StoredFileMarker> {
    scenario.ledgers[1..]
        .iter()
        .map(|row| {
            let marker = store::StoredFileMarker {
                file_id: row.file.file_id,
                version: row.file.version.clone(),
                unit: row.units[0],
                intro_start_ms: Some(7_000),
                intro_end_ms: Some(77_000),
                outro_start_ms: None,
                outro_end_ms: None,
                source: "previous_analysis".into(),
                locked: false,
                updated_at: 1,
            };
            scenario.store.put_fixture_marker(&marker).unwrap();
            scenario
                .store
                .get_file_marker(marker.file_id, marker.unit)
                .unwrap()
                .unwrap()
        })
        .collect()
}

async fn ordinary_publication_is_scoped(mode: &str) {
    use api::ScrapeStoreExt;
    let s = scenario().await;
    let mut config = s.store.get_scrape_config().unwrap().setting;
    config.fingerprint_sampling_mode = Some(mode.into());
    s.store.save_scrape_config(&config).unwrap();
    seed_adaptive_references(&s, mode).await;
    let previous = seed_reference_markers(&s);
    probe(&s).await;
    let target = &s.ledgers[0];
    let published = s
        .store
        .get_file_marker(target.file.file_id, target.units[0])
        .unwrap()
        .expect("the leased target must publish so the isolation assertion is meaningful");
    assert_eq!(
        (published.intro_start_ms, published.intro_end_ms),
        (Some(30_000), Some(90_000))
    );
    for marker in previous {
        assert_eq!(
            s.store
                .get_file_marker(marker.file_id, marker.unit)
                .unwrap(),
            Some(marker),
            "an independent comparison reference is outside the ordinary job write scope"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_fullwindow_does_not_publish_to_outside_job_references() {
    ordinary_publication_is_scoped("full_window").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_adaptive_does_not_publish_to_outside_job_references() {
    ordinary_publication_is_scoped("adaptive").await;
}

struct ChangeReference {
    row: domain::FileCoverage,
    changed: AtomicBool,
}
impl FingerprintEngine for ChangeReference {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!("samples are cached or captured through the injected capture engine")
    }
    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        min: f32,
        max: f32,
    ) -> Option<CommonSegment> {
        if !self.changed.swap(true, Ordering::SeqCst) {
            std::fs::write(
                &self.row.file.path,
                "https://example.invalid/reference-replaced.mkv\n",
            )
            .unwrap();
        }
        FixedCandidate.find_common_segment(first, second, min, max)
    }
}

async fn ordinary_checks_reference_source(mode: &str) {
    use api::ScrapeStoreExt;
    let mut s = scenario().await;
    let mut config = s.store.get_scrape_config().unwrap().setting;
    config.fingerprint_sampling_mode = Some(mode.into());
    s.store.save_scrape_config(&config).unwrap();
    seed_adaptive_references(&s, mode).await;
    s.manager.shutdown_workers().await.unwrap();
    let engine = Arc::new(ChangeReference {
        row: s.ledgers[1].clone(),
        changed: AtomicBool::new(false),
    });
    s.manager = Arc::new(
        ProbeManager::with_engines(s.store.clone(), engine.clone(), Arc::new(FixedCandidate))
            .unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    let previous = seed_reference_markers(&s);
    assert!(s.manager.try_start_workers());
    assert!(s.manager.enqueue(unit(&s.ledgers[0], false)));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !engine.changed.load(Ordering::SeqCst) {
            advance_analysis(&s);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        loop {
            if s.store
                .active_probe_unit_for_ledger(&s.ledgers[0].file.file_id.to_string())
                .unwrap()
                .is_some_and(|unit| unit.status == "queued")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(engine.changed.load(Ordering::SeqCst));
    let target = &s.ledgers[0];
    assert!(
        s.store
            .get_file_marker(target.file.file_id, target.units[0])
            .unwrap()
            .is_none(),
        "changing an out-of-job reference must reject the leased target's result"
    );
    for marker in previous {
        assert_eq!(
            s.store
                .get_file_marker(marker.file_id, marker.unit)
                .unwrap(),
            Some(marker)
        );
    }
    let job = s
        .store
        .latest_probe_job_for_scope(&format!("ledger:{}", target.file.file_id))
        .unwrap()
        .unwrap();
    assert_ne!(
        job.status, "succeeded",
        "a stale comparison cannot be reported as published"
    );
    finish_probe(&s).await;
    assert_eq!(
        latest_outcome(&s)["status"],
        "verified",
        "surviving independent references can publish a fresh result"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_fullwindow_rejects_changed_outside_job_reference() {
    ordinary_checks_reference_source("full_window").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_adaptive_rejects_changed_outside_job_reference() {
    ordinary_checks_reference_source("adaptive").await;
}
