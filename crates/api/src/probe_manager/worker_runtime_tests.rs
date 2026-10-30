use super::*;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn fixture() -> (tempfile::TempDir, Arc<crate::Store>, ProbeUnit) {
    let directory = tempfile::tempdir().unwrap();
    let store = crate::Store::open(directory.path().join("data")).unwrap();
    let root = directory.path().join("shows");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            domain::MediaKind::Tv,
            "Shows",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&library.id, true, true)
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
    (directory, Arc::new(store), unit)
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
        .as_ref()
        .enqueue_chapter_frame_task(&obligation)
        .unwrap();
    assert!(
        store
            .as_ref()
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
        occupied
            .with_writer(store::Db::Library, |_| {
                held.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(500));
                Ok(())
            })
            .unwrap();
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let started = Instant::now();
    let task = tokio::spawn(async move { service.shutdown().await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let timer_elapsed = started.elapsed();
    task.await.unwrap().unwrap();
    holder.join().unwrap();
    assert_eq!(
        store.as_ref().pending_chapter_frame_tasks(10).unwrap(),
        vec![obligation]
    );
    assert!(
        timer_elapsed < Duration::from_millis(200),
        "chapter shutdown parked current-thread runtime for {timer_elapsed:?}"
    );
}
