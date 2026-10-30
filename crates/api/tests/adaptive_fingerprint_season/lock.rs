use super::*;
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct ContendedModelPublication {
    writer: Arc<Store>,
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
    holder: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl marker::FingerprintEngine for ContendedModelPublication {
    fn extract_at(
        &self,
        path: &std::path::Path,
        start: u32,
        duration: u32,
        control: &marker::WorkControl,
    ) -> Result<marker::AudioFingerprint, String> {
        marker::FingerprintEngine::extract_at(&SharedMatchEngine, path, start, duration, control)
    }

    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        min: f32,
        max: f32,
    ) -> Option<marker::CommonSegment> {
        if let Some(entered) = self.entered.lock().take() {
            let writer = self.writer.clone();
            let release = self.release.lock().take().unwrap();
            let (held, ready) = mpsc::channel();
            *self.holder.lock() = Some(std::thread::spawn(move || {
                let _guard =
                    crate::writer_hold::WriterHold::acquire(writer.clone(), store::Db::Library);
                held.send(()).unwrap();
                let _ = entered.send(());
                let _ = release.recv_timeout(Duration::from_secs(3));
            }));
            ready.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        marker::FingerprintEngine::find_common_segment(&SharedMatchEngine, first, second, min, max)
    }

    fn find_common_segments(
        &self,
        first: &[u32],
        second: &[u32],
        min: f32,
        max: f32,
    ) -> Vec<marker::CommonSegment> {
        self.find_common_segment(first, second, min, max)
            .into_iter()
            .collect()
    }
}

fn season_fixture() -> (tempfile::TempDir, Arc<Store>, MediaId, Vec<ProbeUnit>) {
    let directory = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(directory.path().join("data")).unwrap();
    let media_id = MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Publication timer".into(),
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
    let root = directory.path().join("shows");
    std::fs::create_dir_all(&root).unwrap();
    store::test_support::reconfigure_default_root(&store, MediaKind::Tv, &root).unwrap();
    let units = (1..=2)
        .map(|episode| {
            let path = root.join(format!("Show.S01E{episode:02}.mkv"));
            std::fs::write(&path, b"finite source").unwrap();
            ProbeUnit {
                row: publish_coverage(&store, media_id, &path, episode),
                kind: MediaKind::Tv,
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: true,
                marker_refresh_id: None,
                job_id: None,
            }
        })
        .collect();
    (directory, Arc::new(store), media_id, units)
}

#[test]
fn public_adaptive_model_publication_does_not_park_the_executor() {
    let (_directory, store, media_id, units) = season_fixture();
    let (entered, comparison) = tokio::sync::oneshot::channel();
    let (release, holder_release) = mpsc::channel();
    let matcher = Arc::new(ContendedModelPublication {
        writer: store.clone(),
        entered: Mutex::new(Some(entered)),
        release: Mutex::new(Some(holder_release)),
        holder: Mutex::new(None),
    });
    let manager = Arc::new(
        ProbeManager::with_engines(store, matcher.clone(), Arc::new(SharedMatchEngine)).unwrap(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (elapsed, outcome) = runtime.block_on(async {
        let began = Instant::now();
        let task = tokio::spawn(async move {
            api::probe_manager::season::run_adaptive_season_pipeline(
                &manager,
                "publication-timer",
                media_id,
                1,
                &units,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), comparison)
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let elapsed = began.elapsed();
        let _ = release.send(());
        (elapsed, task.await.unwrap())
    });
    matcher.holder.lock().take().unwrap().join().unwrap();
    assert!(
        elapsed < Duration::from_millis(200),
        "public adaptive publication parked Tokio for {elapsed:?}"
    );
    assert!(
        outcome.is_ok(),
        "publication did not recover after writer release: {outcome:?}"
    );
}
