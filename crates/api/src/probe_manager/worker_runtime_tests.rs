use super::*;
use parking_lot::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn fixture() -> (tempfile::TempDir, Arc<Mutex<crate::Store>>, ProbeUnit) {
    let directory = tempfile::tempdir().unwrap();
    let store = crate::Store::open(directory.path().join("data")).unwrap();
    let root = directory.path().join("shows");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            domain::MediaKind::Tv,
            "Shows",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "Show".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let path = root.join("Show.S01E01.mkv");
    std::fs::write(&path, b"fixture").unwrap();
    let row =
        super::super::test_support::coverage(media.id, path.to_str().unwrap(), Some(1), Some(1));
    let row = super::super::test_support::publish(&store, &row);
    let unit = ProbeUnit {
        row,
        kind: domain::MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: false,
        marker_refresh_id: None,
        job_id: None,
    };
    (directory, Arc::new(Mutex::new(store)), unit)
}

struct ContendedPublication {
    store: Arc<Mutex<crate::Store>>,
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    holder: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl marker::FingerprintEngine for ContendedPublication {
    fn extract_at(&self, _: &std::path::Path, _: u32, _: u32) -> Result<Vec<u32>, String> {
        let store = self.store.clone();
        let entered = self.entered.lock().take().unwrap();
        let (held, ready) = mpsc::channel();
        *self.holder.lock() = Some(std::thread::spawn(move || {
            let _guard = store.lock();
            held.send(()).unwrap();
            let _ = entered.send(());
            std::thread::sleep(Duration::from_millis(500));
        }));
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        Ok(vec![1, 2, 3])
    }

    fn find_common_segment(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<marker::CommonSegment> {
        None
    }
}

#[tokio::test(flavor = "current_thread")]
async fn fingerprint_publication_contention_does_not_park_the_executor() {
    let (_directory, store, unit) = fixture();
    let (entered, capture) = tokio::sync::oneshot::channel();
    let engine = Arc::new(ContendedPublication {
        store: store.clone(),
        entered: Mutex::new(Some(entered)),
        holder: Mutex::new(None),
    });
    let manager = Arc::new(ProbeManager::with_fingerprint_engine(
        store.clone(),
        engine.clone(),
    ));
    let file_id = unit.row.file.file_id;
    let work = probe::FingerprintWork {
        source_version: unit.row.file.version.source_version.clone(),
        unit,
        media_duration_ms: Some(1_000),
        duration_secs: 180,
        start_job: false,
    };
    let started = Instant::now();
    let worker = manager.clone();
    let task = tokio::spawn(async move { process_fingerprint(&worker, work).await });
    tokio::time::timeout(Duration::from_secs(2), capture)
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let timer_elapsed = started.elapsed();
    let available_during_publication = manager.dispatch_work.available_permits();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    engine.holder.lock().take().unwrap().join().unwrap();
    assert!(
        store
            .lock()
            .get_fingerprint_cache(&file_id.to_string())
            .unwrap()
            .is_some()
    );
    assert_eq!(
        available_during_publication, 11,
        "the pipeline must own its blocking permit through publication"
    );
    assert_eq!(manager.dispatch_work.available_permits(), 12);
    assert!(
        timer_elapsed < Duration::from_millis(200),
        "fingerprint publication parked current-thread runtime for {timer_elapsed:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn chapter_shutdown_recovers_claims_without_parking_the_executor() {
    let (_directory, store, unit) = fixture();
    let obligation = store::ChapterFrameTask {
        id: "shutdown-recovery".into(),
        file_id: unit.row.file.file_id,
        source_version: unit.row.file.version.source_version,
        targets: vec![(0, 1_000)],
        targets_hash: "fixture".into(),
    };
    store
        .lock()
        .enqueue_chapter_frame_task(&obligation)
        .unwrap();
    assert!(
        store
            .lock()
            .claim_chapter_frame_task(&obligation.id)
            .unwrap()
    );
    let service = crate::chapter_frame_service::ChapterFrameService::new(
        store.clone(),
        Arc::new(crate::library_lifecycle::LibraryWorkRegistry::default()),
    );
    let (held, ready) = mpsc::channel();
    let occupied = store.clone();
    let holder = std::thread::spawn(move || {
        let _guard = occupied.lock();
        held.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let started = Instant::now();
    let task = tokio::spawn(async move { service.shutdown().await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let timer_elapsed = started.elapsed();
    task.await.unwrap().unwrap();
    holder.join().unwrap();
    assert_eq!(
        store.lock().pending_chapter_frame_tasks(10).unwrap(),
        vec![obligation]
    );
    assert!(
        timer_elapsed < Duration::from_millis(200),
        "chapter shutdown parked current-thread runtime for {timer_elapsed:?}"
    );
}
