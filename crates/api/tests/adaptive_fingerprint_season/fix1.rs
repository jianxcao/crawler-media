use super::*;
use std::sync::{atomic::Ordering, mpsc};
use std::time::Duration;

struct GatedMatch {
    first: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl marker::FingerprintEngine for GatedMatch {
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
        if let Some(first) = self.first.lock().take() {
            first();
        }
        marker::FingerprintEngine::find_common_segment(&SharedMatchEngine, a, b, min, max)
    }
}

async fn wait_until(label: &str, mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|error| panic!("{label}: {error}"));
}

fn enqueue(manager: &ProbeManager, row: &FileCoverage) -> String {
    manager
        .enqueue_direct_fingerprint(
            unit(row.clone()),
            Some(1_800_000),
            row.file.version.source_version.clone(),
            180,
        )
        .unwrap()
}

fn ready(store: &Store, rows: &[FileCoverage]) -> bool {
    rows.iter().all(|row| {
        store
            .active_probe_unit_for_ledger(&row.file.file_id.to_string())
            .unwrap()
            .is_some_and(|unit| unit.stage == store::ProbeDispatchStage::Analysis)
    })
}

#[tokio::test]
async fn refresh_busy_publication_retains_one_prepared_all_season_batch() {
    let (_temp, store, previous, clock, media, root) = setup();
    previous.shutdown_workers().await.unwrap();
    let mut config = store.get_scrape_config().unwrap().setting;
    config.fingerprint_sampling_mode = Some("full_window".into());
    store.save_scrape_config(&config).unwrap();
    let held = Arc::new(Mutex::new(None));
    let hold_slot = held.clone();
    let writer = store.clone();
    let matcher = Arc::new(GatedMatch {
        first: Mutex::new(Some(Box::new(move || {
            *hold_slot.lock() = Some(crate::writer_hold::WriterHold::acquire(
                writer,
                store::Db::Library,
            ));
        }))),
    });
    let manager = Arc::new(
        ProbeManager::with_engines(store.clone(), matcher, Arc::new(SharedMatchEngine)).unwrap(),
    );
    manager.set_clock(clock.clone());
    manager.try_start_workers();
    let rows = (1..=2)
        .flat_map(|season| (1..=3).map(move |episode| (season, episode)))
        .map(|(season, episode)| row(&store, media, &root, season, episode))
        .collect::<Vec<_>>();
    let ids = rows
        .iter()
        .map(|row| row.file.file_id.to_string())
        .collect::<Vec<_>>();
    let specs = ids
        .iter()
        .map(|id| store::ProbeJobUnitSpec {
            ledger_id: id,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: true,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        })
        .collect::<Vec<_>>();
    let job = "busy-all-seasons".to_owned();
    assert!(
        store
            .create_probe_job(
                &job,
                "marker_refresh",
                &media.to_string(),
                None,
                "all-seasons",
                &specs
            )
            .unwrap()
    );
    manager.wake_dispatch();
    wait_until("all refresh captures ready", || ready(&store, &rows)).await;
    clock.set(2_000);
    manager.wake_dispatch();
    wait_until("both seasons prepared", || {
        manager.comparison_runs.load(Ordering::SeqCst) == 2
    })
    .await;
    let prepared = manager.timing_snapshot(&job).unwrap();
    assert!(prepared.raw_pairs_computed.unwrap() > 0);
    // Exceed the real one-second writer admission bound after both seasons were prepared.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    drop(held.lock().take());
    drain(&store, &manager, &clock, &rows).await;
    assert_eq!(
        store.get_probe_job(&job).unwrap().unwrap().status,
        "succeeded"
    );
    assert_eq!(
        manager.comparison_runs.load(Ordering::SeqCst),
        2,
        "publication retries cannot rerun either season"
    );
    let published = manager.timing_snapshot(&job).unwrap();
    assert_eq!(
        (published.raw_pairs_computed, published.raw_pairs_reused),
        (prepared.raw_pairs_computed, prepared.raw_pairs_reused)
    );
    for row in &rows {
        verified_intervals(&store, row);
    }
    manager.shutdown_workers().await.unwrap();
}

#[tokio::test]
async fn arrival_during_running_analysis_keeps_original_due_time() {
    let (_temp, store, previous, clock, media, root) = setup();
    previous.shutdown_workers().await.unwrap();
    let (started, running) = tokio::sync::oneshot::channel();
    let (release, blocked) = mpsc::channel();
    let matcher = Arc::new(GatedMatch {
        first: Mutex::new(Some(Box::new(move || {
            started.send(()).unwrap();
            blocked.recv_timeout(Duration::from_secs(5)).unwrap();
        }))),
    });
    let manager = Arc::new(
        ProbeManager::with_engines(store.clone(), matcher, Arc::new(SharedMatchEngine)).unwrap(),
    );
    manager.set_clock(clock.clone());
    manager.try_start_workers();
    let mut rows = (1..=3)
        .map(|episode| row(&store, media, &root, 1, episode))
        .collect::<Vec<_>>();
    for row in &rows {
        enqueue(&manager, row);
    }
    wait_until("initial captures ready", || ready(&store, &rows)).await;
    // Let the existing bounded horizon observe the initial handoffs at t=0.
    manager.wake_dispatch();
    tokio::time::sleep(Duration::from_millis(150)).await;
    clock.set(2_000);
    manager.wake_dispatch();
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();
    clock.set(3_000);
    let later = row(&store, media, &root, 1, 4);
    let job = enqueue(&manager, &later);
    wait_until("later capture handed off during comparison", || {
        ready(&store, std::slice::from_ref(&later))
    })
    .await;
    clock.set(13_000);
    manager.wake_dispatch();
    release.send(()).unwrap();
    wait_until("overdue later target finishes at fixed clock", || {
        !store.get_probe_job(&job).unwrap().unwrap().is_active()
    })
    .await;
    assert_eq!(
        clock.now_ms(),
        13_000,
        "overdue follow-up must drain without a new quiet advance"
    );
    rows.push(later);
    for row in &rows {
        verified_intervals(&store, row);
    }
    manager.shutdown_workers().await.unwrap();
}
