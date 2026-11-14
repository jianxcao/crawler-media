use super::*;
use crate::probe_manager::ProbeUnit;
use parking_lot::Mutex;
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
    let row = crate::probe_manager::test_support::coverage(
        media.id,
        path.to_str().unwrap(),
        Some(1),
        Some(1),
    );
    let row = crate::probe_manager::test_support::publish(&store, &row);
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

struct ContendedPublication {
    store: Arc<crate::Store>,
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    holder: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl marker::FingerprintEngine for ContendedPublication {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _control: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        let store = self.store.clone();
        let entered = self.entered.lock().take().unwrap();
        let (held, ready) = mpsc::channel();
        *self.holder.lock() = Some(std::thread::spawn(move || {
            store
                .with_writer(store::Db::Library, |_| {
                    held.send(()).unwrap();
                    let _ = entered.send(());
                    std::thread::sleep(Duration::from_millis(500));
                    Ok(())
                })
                .unwrap();
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
    let manager = Arc::new(
        ProbeManager::with_engines(store.clone(), engine.clone(), engine.clone()).unwrap(),
    );
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
            .as_ref()
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

impl marker::FingerprintCaptureEngine for ContendedPublication {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        let words = marker::FingerprintEngine::extract_at(
            self,
            &request.path,
            (request.window.start_ms / 1000) as u32,
            (request.window.duration_ms() / 1000) as u32,
            &marker::WorkControl::default(),
        )
        .map_err(|message| marker::CaptureFailure {
            kind: marker::CaptureFailureKind::Decode,
            message,
            metrics: marker::CaptureMetrics::default(),
        })?;
        Ok(marker::CapturedFingerprint {
            window: request.window.clone(),
            words,
            pcm_duration_ms: Some(request.window.duration_ms()),
            metrics: marker::CaptureMetrics {
                time_mapping_version: 2,
                ..Default::default()
            },
        })
    }
}
