//! Bounded probe dispatch: a 100k durable backlog must recover through ID
//! windows (metadata ≤ 64, fingerprint ≤ 16), keep every row pending or
//! terminal, and refuse a second worker start in the same runtime. Capture is
//! a gated fake; no chromaprint process starts.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use api::Store;
use api::probe_manager::ProbeManager;
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use marker::fingerprint::capture_types::{
    CaptureFailure, CaptureMetrics, CaptureRequest, CapturedFingerprint, FingerprintCaptureEngine,
};
use marker::{AudioFingerprint, CommonSegment, FingerprintEngine};
use store::test_support::{FixtureFile, publish_fixture_file};

const METADATA_PENDING: usize = 100_000;
const FINGERPRINT_PENDING: usize = 100_000;

struct Gate {
    state: Mutex<(usize, bool)>,
    changed: Condvar,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new((0, false)),
            changed: Condvar::new(),
        })
    }

    fn entered(&self) -> usize {
        self.state.lock().0
    }

    fn release(&self) {
        self.state.lock().1 = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let mut state = self.state.lock();
        state.0 += 1;
        self.changed.notify_all();
        self.changed
            .wait_while_for(&mut state, |state| !state.1, Duration::from_secs(30));
        assert!(state.1, "test must release the probe capture gate");
    }
}

struct GatedCapture {
    gate: Arc<Gate>,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl GatedCapture {
    fn new(gate: Arc<Gate>) -> Self {
        Self {
            gate,
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        }
    }

    fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }
}

impl FingerprintEngine for GatedCapture {
    fn extract_at(
        &self,
        _path: &std::path::Path,
        _start_secs: u32,
        _duration_secs: u32,
        _control: &marker::WorkControl,
    ) -> Result<AudioFingerprint, String> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.gate.wait();
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(vec![1, 2, 3, 4])
    }

    fn find_common_segment(
        &self,
        _first: &[u32],
        _second: &[u32],
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Option<CommonSegment> {
        None
    }
}

impl FingerprintCaptureEngine for GatedCapture {
    fn capture_window(
        &self,
        request: &CaptureRequest,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.gate.wait();
        self.active.fetch_sub(1, Ordering::SeqCst);
        let duration_ms = request.window.duration_ms();
        Ok(CapturedFingerprint {
            window: request.window.clone(),
            words: vec![1, 2, 3, 4],
            pcm_duration_ms: Some(duration_ms),
            metrics: CaptureMetrics {
                elapsed_ms: 1,
                pcm_bytes: 16,
                ..Default::default()
            },
        })
    }
}

fn durable_counts(store: &Store) -> (i64, i64) {
    let path = store
        .db_paths()
        .into_iter()
        .find(|path| path.ends_with("library.db"))
        .unwrap();
    let connection = rusqlite::Connection::open(path).unwrap();
    let pending: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM probe_job_units WHERE status IN ('queued','running')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let terminal: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM probe_job_units
             WHERE status IN ('succeeded','failed','cancelled','partial')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    (pending, terminal)
}

fn seed_backlog(store: &Store, real_ledger: &str, root: &std::path::Path) {
    let metadata: Vec<String> = std::iter::once(real_ledger.to_string())
        .chain((1..METADATA_PENDING).map(|index| format!("ledger-m-{index:06}")))
        .collect();
    let metadata_specs: Vec<_> = metadata
        .iter()
        .map(|id| store::ProbeJobUnitSpec {
            ledger_id: id,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: false,
            reuse_media_info_cache: true,
        })
        .collect();
    assert!(
        store
            .create_probe_job(
                "metadata-job",
                "media_probe",
                "media-1",
                None,
                "scope-metadata",
                &metadata_specs,
            )
            .unwrap()
    );

    let fingerprint: Vec<String> = (0..FINGERPRINT_PENDING)
        .map(|_| LedgerId::new().to_string())
        .collect();
    record_fingerprint_owners(store, real_ledger, &fingerprint, root);
    let fingerprint_specs: Vec<_> = fingerprint
        .iter()
        .map(|id| store::ProbeJobUnitSpec {
            ledger_id: id,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: false,
            reuse_media_info_cache: true,
        })
        .collect();
    assert!(
        store
            .create_probe_job(
                "fingerprint-job",
                "fingerprint_probe",
                "media-1",
                None,
                "scope-fingerprint",
                &fingerprint_specs,
            )
            .unwrap()
    );
}

fn record_fingerprint_owners(
    store: &Store,
    template: &str,
    ids: &[String],
    root: &std::path::Path,
) {
    let database = store
        .db_paths()
        .into_iter()
        .find(|path| path.ends_with("library.db"))
        .unwrap();
    let mut connection = rusqlite::Connection::open(database).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();
    let tx = connection.transaction().unwrap();
    {
        // The backlog exercises durable dispatch, so bulk-record owned file
        // facts without making 100k source files or bypassing job admission.
        let mut insert = tx
            .prepare(
                "INSERT INTO library_files (
                 file_id, library_id, root_id, media_id, kind, path_key, display_path,
                 presence_state, publication_state, file_revision, source_version,
                 quality_source, confidence)
             SELECT ?2, library_id, root_id, media_id, kind, ?3, ?3,
                    'present', 'published', 1, source_version, quality_source, confidence
             FROM library_files WHERE file_id=?1",
            )
            .unwrap();
        for id in ids {
            let path = root.join(format!("{id}.mkv"));
            assert_eq!(
                insert
                    .execute(rusqlite::params![template, id, path.to_str().unwrap()])
                    .unwrap(),
                1
            );
        }
    }
    tx.commit().unwrap();
}

fn seed_backlog_fixture(temp: &tempfile::TempDir, media_id: MediaId, ledger_id: LedgerId) {
    let video = temp.path().join("window.mkv");
    std::fs::write(&video, b"not-a-real-video").unwrap();
    {
        let store = Store::open(temp.path()).unwrap();
        store
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Tv,
                title: "Window Fixture".into(),
                year: None,
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let root = temp.path().to_string_lossy().into_owned();
        let library = store
            .create_library(MediaKind::Tv, "TV", &[root.as_str()], "everyone", true, &[])
            .unwrap();
        store
            .set_library_intro_settings(&library.id, true, true)
            .unwrap();
        publish_fixture_file(
            &store,
            &FixtureFile {
                file_id: Some(ledger_id),
                media_id,
                path: video.to_string_lossy().into_owned(),
                season: Some(1),
                episode: Some(1),
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
                ..Default::default()
            },
        )
        .unwrap();
        store
            .put_file_meta_versioned(
                &ledger_id.to_string(),
                &library::Tracks::default(),
                Some("seed-version"),
                Some(1_200_000),
            )
            .unwrap();
        seed_backlog(&store, &ledger_id.to_string(), temp.path());
    }
}

#[tokio::test]
async fn bounded_windows_preserve_a_large_durable_backlog() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let media_id = MediaId::new();
    let ledger_id = LedgerId::new();
    seed_backlog_fixture(&temp, media_id, ledger_id);

    let original = (METADATA_PENDING + FINGERPRINT_PENDING) as i64;
    let store = Arc::new(Mutex::new(Store::open(temp.path()).unwrap()));
    let gate = Gate::new();
    gate.release();
    let capture = Arc::new(GatedCapture::new(gate.clone()));
    let manager = Arc::new(
        ProbeManager::with_engines(store.clone(), capture.clone(), capture.clone()).unwrap(),
    );

    let metadata_pending = store
        .lock()
        .pending_probe_dispatch_ids(store::ProbeDispatchStage::Metadata, 64)
        .unwrap()
        .len();
    let fingerprint_pending = store
        .lock()
        .pending_probe_dispatch_ids(store::ProbeDispatchStage::Fingerprint, 16)
        .unwrap()
        .len();
    assert!(metadata_pending <= 64, "metadata window {metadata_pending}");
    assert!(
        fingerprint_pending <= 16,
        "fingerprint window {fingerprint_pending}"
    );
    // Recovery must not have pulled the whole backlog into the in-memory set.
    assert!(
        manager.queued_ledger_count() <= 64 + 16,
        "in-memory dispatch ids {} exceed the two windows",
        manager.queued_ledger_count()
    );

    assert!(manager.try_start_workers());
    tokio::time::timeout(Duration::from_secs(5), async {
        while manager.worker_count() < 4 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("metadata and fingerprint workers must start");
    let workers_after_first = manager.worker_count();
    assert!(manager.try_start_workers());
    assert_eq!(
        manager.worker_count(),
        workers_after_first,
        "a second start in the same runtime must not add workers"
    );
    let drained = Instant::now();
    loop {
        let (pending, terminal) = durable_counts(&store.lock());
        if pending + terminal == original && capture.max_active() <= 1 {
            break;
        }
        if drained.elapsed() > Duration::from_secs(30) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    manager.shutdown_workers().await.unwrap();
    let (pending, terminal) = durable_counts(&store.lock());
    assert_eq!(
        pending + terminal,
        original,
        "durable pending plus terminal must equal the original count"
    );
    assert!(
        capture.max_active() <= 1,
        "fingerprint stays on its single worker, saw {}",
        capture.max_active()
    );
    assert_eq!(
        manager.queued_ledger_count(),
        0,
        "shutdown drains the in-memory window"
    );
}

#[path = "probe_backlog_capacity/real_window.rs"]
mod real_window;
